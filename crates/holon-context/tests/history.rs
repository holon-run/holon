use holon_context::history::{
    render_turn_record_projection_with_transcript_from_snapshot, RecentTurnProjectionMode,
    TurnHistoryObservations,
};
use holon_domain::{
    AuthorityClass, BriefKind, BriefRecord, IdleInputBatchItem, LiveInputBatchRecord,
    LiveInputSafePoint, MessageBody, MessageEnvelope, MessageKind, MessageOrigin, Priority,
    ToolCommandProjection, ToolContextProjection, ToolExecutionStatus, TranscriptEntry,
    TranscriptEntryKind, TurnRecord, TurnReplayProvenance, TurnTriggerSummary,
};
use serde_json::json;

struct CapturedTurn<'a> {
    owned_message_ids: &'a [&'a str],
    trigger_summary_valid: bool,
}

impl TurnHistoryObservations for CapturedTurn<'_> {
    fn message_belongs_to_turn(&self, _turn: &TurnRecord, id: &str) -> bool {
        self.owned_message_ids.contains(&id)
    }

    fn brief_text<'a>(&'a self, brief: &'a BriefRecord) -> &'a str {
        &brief.text
    }

    fn trigger_summary_valid(&self, _turn: &TurnRecord) -> bool {
        self.trigger_summary_valid
    }
}

fn recent_turn_live_follower_fixture() -> (TurnRecord, MessageEnvelope, MessageEnvelope) {
    let mut anchor = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "canonical trigger body".into(),
        },
    );
    anchor.turn_id = Some("ancestor-turn".into());
    let mut follower = anchor.clone();
    follower.id = "owned-live-follower".into();
    follower.turn_id = Some("replayed-turn".into());
    follower.body = MessageBody::Text {
        text: "owned-live-follower-body".into(),
    };
    let mut turn = TurnRecord::new("default", "replayed-turn", 2);
    turn.trigger = Some(TurnTriggerSummary::from_message(&anchor));
    turn.input_message_ids = vec![anchor.id.clone(), follower.id.clone()];
    turn.replay = Some(TurnReplayProvenance {
        source_message_id: anchor.id.clone(),
        source_turn_id: "ancestor-turn".into(),
        reason: "interrupted_queue_claim_reentry".into(),
        prior_terminal: None,
    });
    let mut batch = LiveInputBatchRecord {
        activation_id: "replayed-activation".into(),
        safe_point: LiveInputSafePoint::new("original-activation", 2),
        cutoff: chrono::Utc::now(),
        max_items: 8,
        max_bytes: 24 * 1024,
        rendered_bytes: 0,
        result_identities: Vec::new(),
        ordinary_message_ids: vec![follower.id.clone()],
        items: vec![IdleInputBatchItem {
            message_id: follower.id.clone(),
            source_ref: format!("message:{}", follower.id),
            rendered: "owned-live-follower-body\n".into(),
        }],
    };
    batch.rendered_bytes = batch.rendered_text().len();
    turn.live_input_batches.push(batch);
    (turn, anchor, follower)
}

#[test]
fn recent_turns_covers_transcript_sequence_and_fallback_references() {
    let turn_id = "turn-sequence-fallbacks";
    let mut operator = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "Run both tools and summarize them.".into(),
        },
    );
    operator.turn_id = Some(turn_id.into());

    let assistant_entry = TranscriptEntry::new(
        "default",
        TranscriptEntryKind::AssistantRound,
        Some(1),
        None,
        json!({
            "turn_id": turn_id,
            "blocks": [{"type": "text", "text": "I ran the requested tools."}]
        }),
    );
    let tool_results_entry = TranscriptEntry::new(
        "default",
        TranscriptEntryKind::ToolResults,
        Some(1),
        None,
        json!({
            "turn_id": turn_id,
            "refs": [{"tool_execution_id": "tool-sequenced"}]
        }),
    );
    let transcript = [assistant_entry.clone(), tool_results_entry];

    let mut sequenced_brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "sequenced brief preview",
        Some(operator.id.clone()),
        None,
    );
    sequenced_brief.id = "brief-sequenced".into();
    sequenced_brief.turn_id = Some(turn_id.into());
    sequenced_brief.finalizes_assistant_round_id = Some(assistant_entry.id.clone());

    let mut fallback_brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "fallback brief preview",
        Some(operator.id.clone()),
        None,
    );
    fallback_brief.id = "brief-fallback".into();
    fallback_brief.turn_id = Some(turn_id.into());

    let make_tool = |id: &str, summary: &str| ToolContextProjection {
        id: id.into(),
        agent_id: "default".into(),
        work_item_id: None,
        turn_index: 1,
        turn_id: Some(turn_id.into()),
        tool_name: "ExecCommand".into(),
        created_at: chrono::Utc::now(),
        authority_class: AuthorityClass::RuntimeInstruction,
        status: ToolExecutionStatus::Success,
        summary: summary.into(),
        command: ToolCommandProjection {
            command_preview: None,
            disposition: None,
            task_id: None,
            exit_status: None,
            has_output_evidence: false,
            truncated: false,
            has_artifact: false,
        },
        batch_total: 0,
        batch_succeeded: 0,
        batch_failed: 0,
        batch_promoted: 0,
        batch_unknown: 0,
    };
    let sequenced_tool = make_tool("tool-sequenced", "sequenced tool summary");
    let unsequenced_tool = make_tool("tool-unsequenced", "unsequenced tool summary");

    let mut turn = TurnRecord::new("default", turn_id, 1);
    turn.input_message_ids = vec![operator.id.clone()];
    turn.produced_brief_ids = vec![sequenced_brief.id.clone(), fallback_brief.id.clone()];
    turn.tool_execution_ids = vec![sequenced_tool.id.clone(), unsequenced_tool.id.clone()];
    turn.trigger = Some(TurnTriggerSummary::from_message(&operator));

    let rendered = render_turn_record_projection_with_transcript_from_snapshot(
        &CapturedTurn {
            owned_message_ids: &[operator.id.as_str()],
            trigger_summary_valid: true,
        },
        &turn,
        std::slice::from_ref(&operator),
        &[sequenced_brief, fallback_brief],
        &[sequenced_tool, unsequenced_tool],
        &transcript,
        None,
        RecentTurnProjectionMode::Continuity,
        20_000,
    )
    .expect("turn with transcript fallbacks should render");

    assert!(
        rendered.contains("assistant: I ran the requested tools. brief_ref=brief:brief-sequenced")
    );
    assert!(rendered.contains("tool: ExecCommand summary=sequenced tool summary"));
    assert!(rendered.contains("tool (unsequenced): ExecCommand summary=unsequenced tool summary"));
    assert!(rendered.contains("retrieval fallbacks:"));
    assert!(rendered.contains("result fallback: brief_ref=brief:brief-fallback"));
}

#[test]
fn recent_turns_explicit_foreign_or_missing_trigger_never_uses_owned_follower() {
    for missing in [false, true] {
        let (turn, anchor, follower) = recent_turn_live_follower_fixture();
        let mut messages = vec![&follower];
        if !missing {
            messages.push(&anchor);
        }
        let rendered = render_turn_record_projection_with_transcript_from_snapshot(
            &CapturedTurn {
                owned_message_ids: &[follower.id.as_str()],
                trigger_summary_valid: missing,
            },
            &turn,
            &messages,
            &[] as &[BriefRecord],
            &[] as &[ToolContextProjection],
            &[] as &[TranscriptEntry],
            None,
            RecentTurnProjectionMode::Continuity,
            20_000,
        )
        .unwrap();
        if missing {
            assert!(
                rendered.contains("  - trigger: trusted operator input"),
                "{rendered}"
            );
        } else {
            assert!(rendered.contains("  - trigger: unavailable"), "{rendered}");
        }
        assert!(!rendered.contains("owned-live-follower"));
        assert!(!rendered.contains("canonical trigger body"));
    }
}

#[test]
fn recent_turns_explicit_owned_trigger_wins_over_earlier_live_follower() {
    let (mut turn, mut anchor, follower) = recent_turn_live_follower_fixture();
    anchor.turn_id = Some(turn.turn_id.clone());
    turn.replay = None;
    turn.input_message_ids = vec![follower.id.clone(), anchor.id.clone()];
    let rendered = render_turn_record_projection_with_transcript_from_snapshot(
        &CapturedTurn {
            owned_message_ids: &[anchor.id.as_str(), follower.id.as_str()],
            trigger_summary_valid: true,
        },
        &turn,
        &[&follower, &anchor],
        &[] as &[BriefRecord],
        &[] as &[ToolContextProjection],
        &[] as &[TranscriptEntry],
        None,
        RecentTurnProjectionMode::Continuity,
        20_000,
    )
    .unwrap();
    assert!(rendered.contains("canonical trigger body"));
    assert!(!rendered.contains("owned-live-follower"));
    assert!(!rendered.contains("  - trigger: unavailable"));
}

#[test]
fn recent_turns_legacy_trigger_fallback_excludes_live_batch_items() {
    for summary_without_id in [false, true] {
        let (mut turn, mut anchor, follower) = recent_turn_live_follower_fixture();
        anchor.turn_id = Some(turn.turn_id.clone());
        turn.replay = None;
        if summary_without_id {
            turn.trigger.as_mut().unwrap().message_id = None;
        } else {
            turn.trigger = None;
        }
        turn.input_message_ids = vec![follower.id.clone(), anchor.id.clone()];
        // Item identity, not optional ordinary-message metadata, excludes live inputs.
        turn.live_input_batches[0].ordinary_message_ids.clear();
        let rendered = render_turn_record_projection_with_transcript_from_snapshot(
            &CapturedTurn {
                owned_message_ids: &[anchor.id.as_str(), follower.id.as_str()],
                trigger_summary_valid: true,
            },
            &turn,
            &[&follower, &anchor],
            &[] as &[BriefRecord],
            &[] as &[ToolContextProjection],
            &[] as &[TranscriptEntry],
            None,
            RecentTurnProjectionMode::Continuity,
            20_000,
        )
        .unwrap();
        assert!(rendered.contains("canonical trigger body"));
        assert!(!rendered.contains("owned-live-follower"));
        assert!(!rendered.contains("  - trigger: unavailable"));
    }
}

#[test]
fn older_recent_turn_operator_input_is_previewed_with_message_ref() {
    let long_tail = "older-operator-tail-hidden-behind-message-ref";
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
            text: format!("{} {long_tail}", "older operator input. ".repeat(40)),
        },
    );
    message.turn_id = Some("turn-old-message-preview".into());
    let mut brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "brief anchor remains visible",
        Some(message.id.clone()),
        None,
    );
    brief.turn_id = Some("turn-old-message-preview".into());
    let mut turn = TurnRecord::new("default", "turn-old-message-preview", 1);
    turn.input_message_ids = vec![message.id.clone()];
    turn.produced_brief_ids = vec![brief.id.clone()];
    turn.trigger = Some(TurnTriggerSummary::from_message(&message));

    let rendered = render_turn_record_projection_with_transcript_from_snapshot(
        &CapturedTurn {
            owned_message_ids: &[message.id.as_str()],
            trigger_summary_valid: true,
        },
        &turn,
        std::slice::from_ref(&message),
        &[brief],
        &[] as &[ToolContextProjection],
        &[] as &[TranscriptEntry],
        None,
        RecentTurnProjectionMode::Older,
        20_000,
    )
    .expect("older turn should render");

    assert!(rendered.contains("operator input preview:"));
    assert!(rendered.contains(&format!(
        "[truncated; full via message_ref=message:{}]",
        message.id
    )));
    assert!(!rendered.contains(long_tail));
    assert!(rendered.contains("brief_ref=brief:"));
}

#[test]
fn continuity_result_truncation_keeps_explicit_brief_ref() {
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
            text: "summarize the long result".to_string(),
        },
    );
    message.turn_id = Some("turn-long-continuity".into());
    let mut brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "long result body ".repeat(200),
        Some(message.id.clone()),
        None,
    );
    brief.id = "brief-long-continuity".into();
    brief.turn_id = Some("turn-long-continuity".into());
    let mut turn = TurnRecord::new("default", "turn-long-continuity", 1);
    turn.input_message_ids = vec![message.id.clone()];
    turn.produced_brief_ids = vec![brief.id.clone()];
    turn.trigger = Some(TurnTriggerSummary::from_message(&message));

    let rendered = render_turn_record_projection_with_transcript_from_snapshot(
        &CapturedTurn {
            owned_message_ids: &[message.id.as_str()],
            trigger_summary_valid: true,
        },
        &turn,
        std::slice::from_ref(&message),
        &[brief],
        &[] as &[ToolContextProjection],
        &[] as &[TranscriptEntry],
        None,
        RecentTurnProjectionMode::Continuity,
        80,
    )
    .expect("turn should render");

    assert!(rendered.contains("Result excerpt"));
    assert!(rendered.contains("[truncated; full via brief_ref=brief:brief-long-continuity]"));
}
