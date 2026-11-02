use super::*;
use crate::types::{
    AdmissionContext, AuthorityClass, MessageBody, MessageDeliverySurface, MessageKind,
    MessageOrigin, Priority, TurnTerminalKind, TurnTerminalSummary,
};

fn recovery_settlement(kind: TurnTerminalKind) -> QueueTransitionCommand {
    let now = Utc::now();
    let mut recovery = MessageEnvelope::new(
        "agent-a",
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "model_lineage_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Next,
        MessageBody::Text {
            text: "provider recovery".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    recovery.causation_id = Some("source".into());
    recovery
        .source_refs
        .insert("source_message_id".into(), "source".into());
    recovery
        .source_refs
        .insert("source_turn_id".into(), "source-turn".into());
    recovery.metadata = Some(serde_json::json!({"provider_recovery": {
        "fallback_model_ref": "anthropic/claude-sonnet-4-6",
        "source_message_id": "source",
        "source_turn_id": "source-turn",
        "source_terminal_kind": kind,
        "source_round": 1
    }}));
    let mut turn = TurnRecord::new("agent-a", "source-turn", 1);
    turn.input_message_ids.push("source".into());
    turn.terminal = Some(TurnTerminalSummary {
        kind,
        reason: None,
        no_brief_reason: None,
        completed_at: now,
        duration_ms: 1,
    });
    QueueTransitionCommand {
        agent_id: "agent-a".into(),
        operation: QueueOperation::Settle,
        mutation: QueueMutation::SettleWithProviderRecovery {
            record: QueueEntryRecord {
                message_id: "source".into(),
                agent_id: "agent-a".into(),
                priority: Priority::Normal,
                status: QueueEntryStatus::Processed,
                created_at: now,
                updated_at: now,
            },
            recovery: Box::new(recovery.clone()),
        },
        scheduler_claim_work_item: None,
        agent_state: None,
        message_evidence: vec![recovery],
        transcript_entries: Vec::new(),
        turn_record: Some(turn),
        audit_events: Vec::new(),
        notify_scheduler: true,
        fault: None,
        brief_evidence: Vec::new(),
    }
}

#[test]
fn unclaimed_input_interruption_preserves_source_without_terminal_authority() -> Result<()> {
    for status in [QueueEntryStatus::Queued, QueueEntryStatus::Interrupted] {
        let mut command = recovery_settlement(TurnTerminalKind::DeferredToFallback);
        let expected = QueueEntryRecord {
            message_id: "source".into(),
            agent_id: "agent-a".into(),
            priority: Priority::Normal,
            status,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let mut record = expected.clone();
        record.status = QueueEntryStatus::Interrupted;
        command.mutation = QueueMutation::CompareAndSet { expected, record };
        command.turn_record = None;
        command.message_evidence.clear();
        validate_queue_operation(&command)?;

        let mut with_turn = command.clone();
        with_turn.turn_record =
            recovery_settlement(TurnTerminalKind::DeferredToFallback).turn_record;
        assert!(validate_queue_operation(&with_turn).is_err());
        let mut with_pending = command.clone();
        with_pending.agent_state = Some(AgentStateMutation {
            expected: None,
            record: Box::new(AgentState::new("agent-a")),
        });
        assert!(validate_queue_operation(&with_pending).is_err());
        let mut wrong_source = command.clone();
        let QueueMutation::CompareAndSet { record, .. } = &mut wrong_source.mutation else {
            unreachable!()
        };
        record.message_id = "another-source".into();
        assert!(validate_queue_operation(&wrong_source).is_err());
        let mut wrong_owner = command.clone();
        wrong_owner.agent_id = "agent-b".into();
        assert!(validate_queue_operation(&wrong_owner).is_err());
        for target in [QueueEntryStatus::Processed, QueueEntryStatus::Aborted] {
            let mut terminal = command.clone();
            let QueueMutation::CompareAndSet { record, .. } = &mut terminal.mutation else {
                unreachable!()
            };
            record.status = target;
            assert!(validate_queue_operation(&terminal).is_err());
        }
        let mut requeue = command;
        requeue.operation = QueueOperation::Requeue;
        assert!(validate_queue_operation(&requeue).is_err());
    }
    Ok(())
}

#[test]
fn recovery_settles_processed_source_without_relaxing_continuity() -> Result<()> {
    for kind in [
        TurnTerminalKind::DeferredToFallback,
        TurnTerminalKind::ProviderFailedNeedsRecovery,
    ] {
        let command = recovery_settlement(kind);
        validate_queue_operation(&command)?;
        let mut interrupted = command.clone();
        let QueueMutation::SettleWithProviderRecovery { record, .. } = &mut interrupted.mutation
        else {
            unreachable!()
        };
        record.status = QueueEntryStatus::Interrupted;
        assert!(validate_queue_operation(&interrupted).is_err());
        let mut wrong_owner = command.clone();
        wrong_owner.turn_record.as_mut().unwrap().agent_id = "agent-b".into();
        assert!(validate_queue_operation(&wrong_owner).is_err());
        let mut wrong_turn = command.clone();
        wrong_turn.turn_record.as_mut().unwrap().turn_id = "another-turn".into();
        assert!(validate_queue_operation(&wrong_turn).is_err());
        let mut missing_evidence = command.clone();
        missing_evidence.message_evidence.clear();
        assert!(validate_queue_operation(&missing_evidence).is_err());
    }
    Ok(())
}

#[test]
fn recovery_queue_and_turn_roll_back_at_terminal_faults() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let db = RuntimeDb::open_and_migrate(
        dir.path().join("runtime.sqlite"),
        dir.path().join("runtime.lock"),
    )?;
    let mut source = MessageEnvelope::new(
        "agent-a",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "source input".into(),
        },
    );
    source.id = "source".into();
    db.evidence().append_message(&source)?;
    let mut command = recovery_settlement(TurnTerminalKind::DeferredToFallback);
    let QueueMutation::SettleWithProviderRecovery { record, recovery } = &command.mutation else {
        unreachable!()
    };
    let recovery_id = recovery.id.clone();
    let mut claimed = record.clone();
    claimed.status = QueueEntryStatus::Dequeued;
    db.queue_entries().upsert(&claimed)?;
    for fault in [
        TransitionFaultPoint::AfterTerminalAgentStateWrite,
        TransitionFaultPoint::AfterTerminalTurnRecordWrite,
        TransitionFaultPoint::AfterCanonicalWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        command.fault = Some(fault);
        let error = db
            .transitions()
            .commit_queue_with_execution_protocol(&command, &ExecutionProtocolTransition::default())
            .unwrap_err();
        assert!(error.to_string().contains(&format!("{fault:?}")), "{error}");
        assert_eq!(db.queue_entries().latest_all()?, vec![claimed.clone()]);
        assert!(db
            .messages()
            .by_id(Some("agent-a"), &recovery_id)?
            .is_none());
        assert!(db.turn_records().recent_for_agent("agent-a", 8)?.is_empty());
    }
    command.fault = None;
    let commit = db
        .transitions()
        .commit_queue_with_execution_protocol(&command, &ExecutionProtocolTransition::default())?;
    assert!(commit.applied);
    assert_eq!(
        db.queue_entries().latest("source")?.unwrap().status,
        QueueEntryStatus::Processed
    );
    assert_eq!(
        db.queue_entries().latest(&recovery_id)?.unwrap().status,
        QueueEntryStatus::Queued
    );
    let stored = db.messages().by_id(Some("agent-a"), &recovery_id)?.unwrap();
    assert_eq!(commit.effects.queued_messages, vec![stored.clone()]);
    assert_eq!(commit.effects.messages, vec![stored]);
    Ok(())
}
