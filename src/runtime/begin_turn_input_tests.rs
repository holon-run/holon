use super::*;
use crate::{
    runtime::tests::support::{context_config, StubProvider},
    types::{MessageKind, MessageOrigin, Priority, TurnTerminalSummary},
};

fn fixture() -> (tempfile::TempDir, RuntimeHandle) {
    let home = tempfile::tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "begin-turn-input",
        home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "begin-turn-input".into(),
        context_config(),
    )
    .unwrap();
    (home, runtime)
}

fn input(turn_id: &str) -> MessageEnvelope {
    let mut message = MessageEnvelope::new(
        "begin-turn-input",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "one exact turn input".into(),
        },
    );
    message.turn_id = Some(turn_id.into());
    message
}

async fn store_qualified_turn(runtime: &RuntimeHandle, turn: &TurnRecord) -> Result<()> {
    let mut mutation = runtime.begin_mutation().await?;
    mutation
        .commit_input_evidence(crate::ids::audit_event_id(), |scope| {
            runtime
                .inner
                .runtime_db
                .turn_records()
                .upsert_in_scope(scope, turn)
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn begin_turn_accepts_preallocated_hint_without_a_started_turn() -> Result<()> {
    let (_home, runtime) = fixture();
    let message = runtime.enqueue(input("turn:preallocated:first")).await?;
    assert!(runtime
        .runtime_db()
        .turn_records()
        .by_id(Some("begin-turn-input"), "turn:preallocated:first")?
        .is_none());
    runtime
        .begin_interactive_turn_with_provenance(Some(&message), None, None, None)
        .await?;
    let state = runtime.agent_state().await?;
    assert_eq!(
        state.current_turn_id.as_deref(),
        Some("turn:preallocated:first")
    );
    let turn = runtime
        .runtime_db()
        .turn_records()
        .by_id(Some(&state.id), state.current_turn_id.as_deref().unwrap())?
        .unwrap();
    assert!(turn.replay.is_none());
    assert_eq!(turn.input_message_ids, vec![message.id.clone()]);
    let mut mutation = runtime.begin_mutation().await?;
    mutation
        .prepare_owner_inputs(Default::default(), Default::default())
        .await?;
    let captured = mutation.capture_input_evidence()?;
    assert_eq!(
        captured.message_turn_keys.get(&message.id),
        Some(&Some("turn:preallocated:first".into()))
    );
    Ok(())
}

#[tokio::test]
async fn begin_turn_reentry_uses_actual_terminal_assignment_instead_of_payload_hint() -> Result<()>
{
    let (_home, runtime) = fixture();
    let message = runtime.enqueue(input("turn:preallocated:source")).await?;
    runtime
        .begin_interactive_turn_with_provenance(Some(&message), None, None, None)
        .await?;
    let first = runtime.agent_state().await?;
    let mut terminal = runtime
        .runtime_db()
        .turn_records()
        .by_id(Some(&first.id), first.current_turn_id.as_deref().unwrap())?
        .unwrap();
    terminal.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Aborted,
        reason: Some("original exact source terminal".into()),
        no_brief_reason: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    });
    store_qualified_turn(&runtime, &terminal).await?;
    // A dispatch may carry a different preallocation. It cannot replace the
    // canonical source relation, nor borrow a different Turn's terminal body.
    let mut reentry = message.clone();
    reentry.turn_id = Some("turn:unrelated:new:hint".into());
    reentry.source_refs.insert(
        "replay_source_turn_id".into(),
        "turn:unrelated:new:hint".into(),
    );
    runtime
        .begin_interactive_turn_with_provenance(Some(&reentry), None, None, None)
        .await?;
    let second = runtime.agent_state().await?;
    assert_ne!(second.current_turn_id, first.current_turn_id);
    assert_ne!(
        second.current_turn_id.as_deref(),
        reentry.turn_id.as_deref()
    );
    let replay = runtime
        .runtime_db()
        .turn_records()
        .by_id(Some(&second.id), second.current_turn_id.as_deref().unwrap())?
        .unwrap();
    assert_eq!(
        replay.replay,
        Some(crate::types::TurnReplayProvenance {
            source_message_id: message.id,
            source_turn_id: terminal.turn_id.clone(),
            reason: "interrupted_queue_claim_reentry".into(),
            prior_terminal: terminal.terminal.clone(),
        })
    );
    assert_eq!(
        runtime
            .runtime_db()
            .turn_records()
            .by_id(Some(&second.id), &terminal.turn_id)?,
        Some(terminal)
    );
    Ok(())
}

#[tokio::test]
async fn begin_turn_unassigned_hint_collision_cannot_import_or_overwrite_another_terminal(
) -> Result<()> {
    let (_home, runtime) = fixture();
    let mut unrelated = TurnRecord::new("begin-turn-input", "turn:unrelated:terminal", 77);
    unrelated.terminal = Some(TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: Some("unrelated private terminal content".into()),
        no_brief_reason: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    });
    store_qualified_turn(&runtime, &unrelated).await?;
    let message = runtime.enqueue(input(&unrelated.turn_id)).await?;
    let before = runtime.agent_state().await?;
    let queue_before = runtime.runtime_db().queue_entries().latest(&message.id)?;
    let (actual_turn_id, actual_turn_index) = runtime
        .begin_interactive_turn_with_provenance(Some(&message), None, None, None)
        .await?;
    assert_ne!(actual_turn_id, unrelated.turn_id);
    assert!(actual_turn_index > unrelated.turn_index);
    let after = runtime.agent_state().await?;
    assert_eq!(
        after.current_turn_id.as_deref(),
        Some(actual_turn_id.as_str())
    );
    assert_eq!(
        runtime.runtime_db().agent_states().latest(&before.id)?,
        Some(after)
    );
    assert_eq!(
        runtime.runtime_db().queue_entries().latest(&message.id)?,
        queue_before
    );
    assert_eq!(
        runtime
            .runtime_db()
            .turn_records()
            .by_id(Some(&before.id), &unrelated.turn_id)?,
        Some(unrelated)
    );
    let actual = runtime
        .runtime_db()
        .turn_records()
        .by_id(Some(&before.id), &actual_turn_id)?
        .unwrap();
    assert_eq!(actual.turn_index, actual_turn_index);
    assert!(actual.terminal.is_none());
    assert!(actual.replay.is_none());
    assert_eq!(actual.input_message_ids, vec![message.id.clone()]);
    let mut mutation = runtime.begin_mutation().await?;
    mutation
        .prepare_owner_inputs(Default::default(), Default::default())
        .await?;
    let captured = mutation.capture_input_evidence()?;
    assert_eq!(
        captured.message_turn_keys.get(&message.id),
        Some(&Some(actual_turn_id))
    );
    Ok(())
}
