use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::{current_read_facts::*, input_evidence_budget::InputEvidenceBudgetLedger},
    runtime_db::{
        current_read_snapshot::CurrentReadSnapshot,
        scheduling_snapshot::SchedulingDomainLoadRequest, task_snapshot::TaskDomainLoadRequest,
        transitions::TransitionCommit,
    },
};
use std::sync::Arc;

fn execute(
    f: &Fixture,
    command: &PreparedInputTransition,
    ledger: &InputEvidenceBudgetLedger,
) -> Result<InputMutationExecution<TransitionCommit>> {
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition_with_domain_requests(
            command,
            &request(),
            ledger,
            &TaskDomainLoadRequest {
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                query: Default::default(),
                byte_limit: CURRENT_READ_MAX_BYTES,
                record_limit: CURRENT_READ_MAX_RECORDS,
            },
            &SchedulingDomainLoadRequest {
                capture: crate::runtime::resident_scheduling::SchedulingCaptureRequest::current(),
                byte_limit: CURRENT_READ_MAX_BYTES,
                record_limit: CURRENT_READ_MAX_RECORDS,
            },
        )
}
fn boundary_fixture() -> Result<(Fixture, InputEvidenceBudgetLedger)> {
    let f = Fixture::new(true)?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let before = state(&f)?;
    let mut after = before.clone();
    after.current_work_item_id = Some("work:dispatch-memory".into());
    let mut command = registration(&f, "wait:current-read-memory");
    command.agent_state = Some(AgentStateMutation {
        expected: Some(Box::new(before)),
        record: Box::new(after),
    });
    let prepared = PreparedInputTransition::wait(
        f.scope.clone(),
        "memory-current-deferred".into(),
        f.at,
        command,
        Default::default(),
        None,
    )?
    .with_dispatch_memory(DispatchMemoryIntent::Deferred {
        binding: DispatchMemoryBinding::from_start(&f.start),
        frozen: f.frozen("first-durable-member"),
    })?;
    assert!(matches!(
        execute(&f, &prepared, &ledger)?,
        InputMutationExecution::Committed { .. }
    ));
    assert_eq!(
        f.read()?.unwrap().obligation().unwrap().frozen(),
        &f.frozen("first-durable-member")
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok((f, ledger))
}
fn current_request() -> CurrentReadRequest {
    CurrentReadRequest::current(CURRENT_READ_MAX_BYTES, CURRENT_READ_MAX_RECORDS)
}
fn row_snapshot(f: &Fixture) -> Result<Vec<(String, String)>> {
    let connection = f.db.connection()?;
    [
        ("state", "SELECT payload_json FROM agent_states WHERE agent_id='dispatch-memory'"),
        ("turn", "SELECT payload_json FROM turn_records WHERE turn_id='turn:dispatch-memory'"),
        ("memory", "SELECT payload_json FROM dispatch_memory_checkpoints WHERE agent_id='dispatch-memory'"),
        ("coverage", "SELECT COALESCE(json_group_array(json_array(agent_id,identity_incarnation,revision,coverage_json)), '[]') FROM current_read_current"),
        ("members", "SELECT COALESCE(json_group_array(json_array(agent_id,identity_incarnation,family,record_id,content_digest,projection_digest,revision)), '[]') FROM current_read_members"),
        ("events", "SELECT COALESCE(json_group_array(json_array(audit_event_id,event_seq,kind,created_at,data_json)), '[]') FROM audit_events"),
        ("sequences", "SELECT COALESCE(json_group_array(json_array(domain,scope_key,last_value)), '[]') FROM runtime_sequences"),
        ("episodes", "SELECT COALESCE(json_group_array(payload_json), '[]') FROM context_episode_anchors"),
        ("memory_receipt", "SELECT COALESCE(json_group_array(result_json), '[]') FROM episode_memory_results"),
        ("whole_receipt", "SELECT COALESCE(json_group_array(json_array(command_nonce,prepared_json,outcome_json)), '[]') FROM input_mutation_receipts"),
    ].into_iter().map(|(name, sql)| {
        Ok((name.to_owned(), connection.query_row(sql, [], |row| row.get(0))?))
    }).collect()
}
fn original_memory_event(snapshot: &CurrentReadSnapshot) -> Result<Arc<QualifiedCurrentReadFact>> {
    let captured = snapshot.resident.capture(&current_request())?;
    captured
        .events
        .iter()
        .find(|event| match &event.fact {
            CurrentReadFact::Event(event) => event.kind == "episode_memory_finalized",
            _ => false,
        })
        .cloned()
        .context("actual memory audit omitted from accepted CurrentRead heads")
}

#[test]
fn embedded_memory_actual_audit_joins_original_current_read_and_whole_receipt() -> Result<()> {
    let (f, ledger) = boundary_fixture()?;
    let prepared = terminal(&f, "incoming-finish-candidate")?;
    let (value, receipt) = match execute(&f, &prepared, &ledger)? {
        InputMutationExecution::Committed { value, receipt } => (value, receipt),
        _ => anyhow::bail!("first original Finish must ACK"),
    };
    let facts = value
        .effects
        .current_read_facts
        .as_ref()
        .context("CurrentRead facts missing")?;
    assert_eq!(facts.revision, facts.base_revision + 1);
    let event = original_memory_event(&facts.snapshot)?;
    let CurrentReadFact::Event(actual) = &event.fact else {
        unreachable!()
    };
    assert_eq!(actual.id, "audit:memory:first-durable-member");
    assert_ne!(
        actual.id,
        f.frozen("incoming-finish-candidate").audit_event_id
    );
    assert!(actual.event_seq > 0);
    assert_eq!(actual.created_at, f.at);
    assert!(receipt.outcome.scopes[0].audit_events.contains(actual));
    let memory =
        f.db.episode_memory_receipt(f.start.key(), None, f.start.invocation())?
            .context("original memory receipt missing")?;
    assert!(memory.outcome.finalized_episode_id.is_some());
    assert_eq!(memory.audit_bindings.len(), 1);
    assert_eq!(memory.audit_bindings[0].audit_event_id, actual.id);
    assert_eq!(memory.audit_bindings[0].event_seq, actual.event_seq);
    let aliases: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM current_read_members WHERE family='event' AND record_id=?1",
        [&f.frozen("incoming-finish-candidate").audit_event_id],
        |r| r.get(0),
    )?;
    assert_eq!(aliases, 0, "unused incoming ID cannot gain qualification");
    assert!(matches!(f.read()?, Some(DispatchMemoryRecord::Done { .. })));
    let before = row_snapshot(&f)?;
    match execute(&f, &prepared, &ledger)? {
        InputMutationExecution::Replayed {
            receipt: replay, ..
        } => assert_eq!(replay, receipt),
        _ => anyhow::bail!("exact original whole receipt replay missing"),
    }
    assert_eq!(row_snapshot(&f)?, before);
    let weak = Arc::downgrade(&event);
    f.db.connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_memory_current_events; ALTER TABLE queue_entries RENAME TO forbidden_memory_current_queue"
    )?;
    let pure = facts.snapshot.resident.capture(&current_request())?;
    assert!(pure
        .events
        .iter()
        .any(|candidate| Arc::ptr_eq(candidate, &event)));
    drop((pure, event, memory, value, receipt));
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn memory_current_audit_rolls_back_with_original_last_receipt_and_retries_once() -> Result<()> {
    let (f, ledger) = boundary_fixture()?;
    let prepared = terminal(&f, "memory-current-receipt-fault")?;
    let before = row_snapshot(&f)?;
    f.db.connection()?.execute_batch(
        "CREATE TRIGGER fail_memory_current_receipt BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='memory-current-receipt-fault' BEGIN SELECT RAISE(ABORT,'memory current whole receipt fault'); END;"
    )?;
    let error = execute(&f, &prepared, &ledger)
        .err()
        .context("last whole receipt fault must rollback")?;
    assert!(format!("{error:#}").contains("memory current whole receipt fault"));
    assert_eq!(row_snapshot(&f)?, before);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    f.db.connection()?
        .execute_batch("DROP TRIGGER fail_memory_current_receipt")?;
    let (value, receipt) = match execute(&f, &prepared, &ledger)? {
        InputMutationExecution::Committed { value, receipt } => (value, receipt),
        _ => anyhow::bail!("unchanged frozen retry must commit"),
    };
    let event =
        original_memory_event(&value.effects.current_read_facts.as_ref().unwrap().snapshot)?;
    let CurrentReadFact::Event(actual) = &event.fact else {
        unreachable!()
    };
    assert_eq!(actual.id, "audit:memory:first-durable-member");
    let after = row_snapshot(&f)?;
    match execute(&f, &prepared, &ledger)? {
        InputMutationExecution::Replayed {
            receipt: replay, ..
        } => assert_eq!(replay, receipt),
        _ => anyhow::bail!("frozen retry must have exact whole ACK"),
    }
    assert_eq!(row_snapshot(&f)?, after);
    let count: usize = f.db.connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE kind='episode_memory_finalized'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1);
    drop((event, value, receipt));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn validated_memory_audit_address_rejects_another_transaction() -> Result<()> {
    let (f, ledger) = boundary_fixture()?;
    let mut foreign = Connection::open_in_memory()?;
    f.db.transaction(|tx| {
        let token = prepare_dispatch_memory_finish_tx(
            tx,
            &DispatchMemoryBinding::from_start(&f.start),
            &f.frozen("unused-incoming"),
        )?;
        assert_eq!(
            token.original_audit_id_tx(tx)?,
            "audit:memory:first-durable-member"
        );
        let other = foreign.transaction()?;
        let error = token
            .original_audit_id_tx(&other)
            .err()
            .context("foreign TX must not freeze this original audit")?;
        assert!(format!("{error:#}").contains("another transaction"));
        Ok(())
    })?;
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
