use super::*;
use crate::runtime_db::{
    input_mutation_receipts::{InputMutationReceipt, ReceiptError},
    transitions::{InputMutationExecution, TurnTerminalTransitionCommand},
};
use crate::types::{TurnTerminalKind, TurnTerminalRecord, TurnTerminalSummary};

fn current(f: &Fixture) -> Result<AgentState> {
    f.db.transaction(|tx| {
        ContextDurableRead::new(tx, &f.scope.agent_id)
            .scope_facts()?
            .state
            .context("fixture State missing")
    })
}
fn updated_work(f: &Fixture, objective: &str) -> Result<WorkItemRecord> {
    let mut work =
        f.db.work_items()
            .latest("work:dispatch-memory")?
            .context("fixture Work missing")?;
    work.revision += 1;
    work.updated_at += chrono::Duration::milliseconds(1);
    work.objective = objective.into();
    Ok(work)
}
fn plain_work(f: &Fixture, nonce: &str, work: &WorkItemRecord) -> Result<PreparedInputTransition> {
    PreparedInputTransition::work(
        f.scope.clone(),
        nonce.into(),
        f.at,
        WorkItemTransitionCommand {
            agent_id: f.scope.agent_id.clone(),
            mutation: WorkItemMutation::Update {
                record: work.clone(),
                expected_revision: work.revision - 1,
            },
            agent_state: None,
            brief_evidence: Vec::new(),
            audit_events: Vec::new(),
            index_changes: Vec::new(),
            notify_scheduler: true,
            fault: None,
        },
        Default::default(),
        false,
    )
}
fn terminal_command(f: &Fixture) -> Result<TurnTerminalTransitionCommand> {
    let before = current(f)?;
    let mut after = before.clone();
    after.status = AgentStatus::AwakeIdle;
    after.current_run_id = None;
    let terminal = TurnTerminalRecord {
        turn_id: f.start.key.actual_dispatch_turn_id.clone(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: f.at,
        duration_ms: 1,
    };
    after.last_turn_terminal = Some(terminal.clone());
    let mut turn =
        f.db.transaction(|tx| {
            ContextDurableRead::new(tx, &f.scope.agent_id).turn_by_id(&terminal.turn_id)
        })?
        .unwrap();
    turn.terminal = Some(TurnTerminalSummary::from_terminal(&terminal));
    Ok(TurnTerminalTransitionCommand {
        agent_id: f.scope.agent_id.clone(),
        agent_state: AgentStateMutation {
            expected: Some(Box::new(before)),
            record: Box::new(after),
        },
        turn_record: turn,
        terminal_tool_executions: Vec::new(),
        audit_events: Vec::new(),
        fault: None,
    })
}
fn plain_terminal(
    f: &Fixture,
    nonce: &str,
    command: &TurnTerminalTransitionCommand,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::terminal(f.scope.clone(), nonce.into(), f.at, command.clone())
}
fn restored(f: &Fixture, plain: PreparedInputTransition) -> Result<PreparedInputTransition> {
    match f
        .db
        .transaction(|tx| load_original_auto_memory_tx(tx, plain.descriptor()))?
    {
        OriginalAutoMemory::Restore(original) => plain.restore_automatic_dispatch_memory(original),
        other => anyhow::bail!("expected original automatic intent, found {other:?}"),
    }
}
fn committed(
    result: InputMutationExecution<crate::runtime_db::transitions::TransitionCommit>,
) -> InputMutationReceipt {
    match result {
        InputMutationExecution::Committed { receipt, .. } => receipt,
        _ => panic!("first original command must commit"),
    }
}
fn count(f: &Fixture, table: &str) -> Result<u64> {
    f.db.transaction(|tx| {
        tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
    })
}
#[test]
fn automatic_deferred_replay_restores_original_frozen_intent_and_rejects_changed_body() -> Result<()>
{
    let f = Fixture::new(true)?;
    let work = updated_work(&f, "frozen detached work")?;
    let plain = plain_work(&f, "auto-detached", &work)?;
    let base = plain.descriptor().clone();
    assert!(matches!(
        f.db.transaction(|tx| load_original_auto_memory_tx(tx, &base))?,
        OriginalAutoMemory::Unseen
    ));
    let original = plain.with_automatic_dispatch_memory(DispatchMemoryIntent::Deferred {
        binding: DispatchMemoryBinding::from_start(&f.start),
        frozen: f.frozen("original-detached"),
    })?;
    let receipt = committed(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
    );
    let obligation = f.read()?.unwrap();
    let retry = restored(&f, plain_work(&f, "auto-detached", &work)?)?;
    assert_eq!(retry.descriptor(), original.descriptor());
    match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&retry)?
    {
        InputMutationExecution::Replayed { receipt: replayed } => assert_eq!(replayed, receipt),
        _ => panic!("retry must not expose old candidate/effects"),
    }
    assert_eq!(f.read()?.unwrap(), obligation);
    assert_eq!(count(&f, "dispatch_memory_auto_commands")?, 1);
    let mut changed = work.clone();
    changed.objective = "changed business same nonce".into();
    let error = restored(&f, plain_work(&f, "auto-detached", &changed)?).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::CommandConflict)
    ));
    assert_eq!(f.db.work_items().latest(&work.id)?, Some(work.clone()));
    let finish_command = terminal_command(&f)?;
    let finish = plain_terminal(&f, "finish-after-deferred", &finish_command)?
        .with_automatic_dispatch_memory(DispatchMemoryIntent::Finish {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("later-finish"),
        })?;
    committed(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&finish)?,
    );
    let done_state = current(&f)?;
    let after_done = restored(&f, plain_work(&f, "auto-detached", &work)?)?;
    assert!(matches!(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&after_done)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(current(&f)?, done_state);
    assert!(matches!(
        f.read()?.unwrap(),
        DispatchMemoryRecord::Done { .. }
    ));
    Ok(())
}
#[test]
fn automatic_finish_replay_after_done_does_not_rewrite_later_state_or_reissue_effects() -> Result<()>
{
    let f = Fixture::new(true)?;
    let command = terminal_command(&f)?;
    let original = plain_terminal(&f, "auto-finish", &command)?.with_automatic_dispatch_memory(
        DispatchMemoryIntent::Finish {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("first-finish"),
        },
    )?;
    let receipt = committed(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
    );
    assert!(matches!(
        f.read()?.unwrap(),
        DispatchMemoryRecord::Done { .. }
    ));
    let mut later = current(&f)?;
    later.pending = 7;
    f.db.agent_states().upsert_for_fixture(&later)?;
    let audits = count(&f, "audit_events")?;
    let retry = restored(&f, plain_terminal(&f, "auto-finish", &command)?)?;
    assert_eq!(retry.descriptor(), original.descriptor());
    match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&retry)?
    {
        InputMutationExecution::Replayed { receipt: replayed } => assert_eq!(replayed, receipt),
        _ => panic!("Done replay must return only the original receipt"),
    }
    assert_eq!(current(&f)?, later);
    assert_eq!(count(&f, "audit_events")?, audits);
    assert_eq!(count(&f, "dispatch_memory_auto_commands")?, 1);
    Ok(())
}
#[test]
fn existing_plain_receipt_prevents_new_automatic_decoration() -> Result<()> {
    let f = Fixture::new(true)?;
    let work = updated_work(&f, "plain original work")?;
    let original = plain_work(&f, "plain-accepted", &work)?;
    committed(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
    );
    assert!(matches!(
        f.db.transaction(|tx| load_original_auto_memory_tx(tx, original.descriptor()))?,
        OriginalAutoMemory::PlainReceipt
    ));
    assert_eq!(count(&f, "dispatch_memory_auto_commands")?, 0);
    assert!(matches!(
        f.read()?.unwrap(),
        DispatchMemoryRecord::Started { .. }
    ));
    Ok(())
}
#[test]
fn automatic_metadata_failure_rolls_back_work_obligation_and_whole_receipt() -> Result<()> {
    let f = Fixture::new(true)?;
    let before = f.db.work_items().latest("work:dispatch-memory")?;
    let seed = f.read()?;
    let receipts = count(&f, "input_mutation_receipts")?;
    let work = updated_work(&f, "must roll back")?;
    let original = plain_work(&f, "auto-rollback", &work)?.with_automatic_dispatch_memory(
        DispatchMemoryIntent::Deferred {
            binding: DispatchMemoryBinding::from_start(&f.start),
            frozen: f.frozen("rollback"),
        },
    )?;
    f.db.transaction(|tx|{tx.execute_batch("CREATE TRIGGER reject_auto BEFORE INSERT ON dispatch_memory_auto_commands BEGIN SELECT RAISE(ABORT,'automatic metadata failure'); END;")?;Ok(())})?;
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&original)
        .is_err());
    assert_eq!(f.db.work_items().latest("work:dispatch-memory")?, before);
    assert_eq!(f.read()?, seed);
    assert_eq!(count(&f, "input_mutation_receipts")?, receipts);
    assert_eq!(count(&f, "dispatch_memory_auto_commands")?, 0);
    Ok(())
}
#[test]
fn automatic_metadata_requires_original_receipt_and_exact_complete_group() -> Result<()> {
    let f = Fixture::new(true)?;
    let work = updated_work(&f, "bound metadata")?;
    let plain = plain_work(&f, "auto-proof", &work)?;
    let base = plain.descriptor().clone();
    let original = plain.with_automatic_dispatch_memory(DispatchMemoryIntent::Deferred {
        binding: DispatchMemoryBinding::from_start(&f.start),
        frozen: f.frozen("proof"),
    })?;
    committed(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
    );
    let different = plain_work(&f, "auto-proof", &work)?.with_scope_group(vec![
        f.scope.clone(),
        InputEvidenceScope {
            agent_id: "readonly-extra".into(),
            identity_incarnation: 1,
        },
    ])?;
    let error =
        f.db.transaction(|tx| load_original_auto_memory_tx(tx, different.descriptor()))
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::GroupConflict)
    ));
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM input_mutation_receipts WHERE command_nonce=?1",
            [base.command_nonce()],
        )?;
        Ok(())
    })?;
    let error =
        f.db.transaction(|tx| load_original_auto_memory_tx(tx, &base))
            .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::CorruptReceipt(_))
    ));
    Ok(())
}
#[test]
fn automatic_intent_is_rehashed_against_the_actual_typed_business_body() -> Result<()> {
    let f = Fixture::new(true)?;
    let work = updated_work(&f, "original hash")?;
    let plain = plain_work(&f, "auto-intent-tamper", &work)?;
    let base = plain.descriptor().clone();
    let original = plain.with_automatic_dispatch_memory(DispatchMemoryIntent::Deferred {
        binding: DispatchMemoryBinding::from_start(&f.start),
        frozen: f.frozen("hash"),
    })?;
    committed(
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition(&original)?,
    );
    // Simulate a conflicting restore with a valid metadata checksum; its final
    // original whole receipt still binds the first frozen intent, not this one.
    f.db.transaction(|tx|{
        let payload:String=tx.query_row("SELECT payload_json FROM dispatch_memory_auto_commands WHERE command_nonce=?1",[base.command_nonce()],|r|r.get(0))?;
        let mut changed:serde_json::Value=serde_json::from_str(&payload)?;
        changed["intent"]["frozen"]["audit_event_id"]="audit:other-frozen".into();
        tx.execute("UPDATE dispatch_memory_auto_commands SET payload_json=?1,metadata_digest=?2 WHERE command_nonce=?3",
            params![serde_json::to_string(&changed)?,canonical_command_digest(&changed)?,base.command_nonce()])?;
        Ok(())
    })?;
    let error = restored(&f, plain_work(&f, "auto-intent-tamper", &work)?).unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ReceiptError>(),
        Some(ReceiptError::CommandConflict)
    ));
    Ok(())
}
