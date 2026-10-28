//! Independent Audit uses the ordinary original command/receipt/Current chain.
use super::*;
use crate::runtime_db::current_read_writes::{CurrentAddressFact, CurrentFamily, CurrentRecord};

fn prepare(
    f: &Fixture,
    event: AuditEvent,
    fault: Option<TransitionFaultPoint>,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::audit(f.scope.clone(), event.id.clone(), f.at, event, fault)
}

#[test]
fn independent_audit_batch_receipt_replays_original_order_without_recreating_pruned_body(
) -> Result<()> {
    let f = Fixture::new()?;
    let events = vec![
        AuditEvent::legacy("original_batch_first", serde_json::json!({"position": 1})),
        AuditEvent::legacy("original_batch_second", serde_json::json!({"position": 2})),
    ];
    let prepared = PreparedInputTransition::audit_many(
        f.scope.clone(),
        crate::ids::audit_event_id(),
        f.at,
        events.clone(),
        None,
    )?;
    let state = f.state()?;
    let receipt = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Committed { value, receipt } => {
            assert_eq!(value.effects.audit_events.len(), 2);
            assert_eq!(value.effects.audit_events[0].id, events[0].id);
            assert_eq!(value.effects.audit_events[1].id, events[1].id);
            assert!(
                value.effects.audit_events[0].event_seq < value.effects.audit_events[1].event_seq
            );
            assert_eq!(
                receipt.outcome.scopes[0].audit_events,
                value.effects.audit_events
            );
            let current = value
                .effects
                .current_read_writes
                .as_ref()
                .context("batch Current ACK")?;
            for event in &events {
                assert!(current
                    .record(&(CurrentFamily::Event, event.id.clone()))
                    .is_some());
            }
            receipt
        }
        _ => panic!("expected original batch commit"),
    };
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id=?1",
            [&events[0].id],
        )?;
        Ok(())
    })?;
    match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Replayed {
            receipt: replay, ..
        } => assert_eq!(replay, receipt),
        _ => panic!("expected original batch replay"),
    }
    assert!(!f.db.audit_events().has_event_by_id(&events[0].id)?);
    assert!(f.db.audit_events().has_event_by_id(&events[1].id)?);
    assert_eq!(f.state()?, state);
    assert_eq!(f.receipt_count()?, 1);
    Ok(())
}

#[test]
fn independent_audit_original_receipt_replay_never_rewrites_state_or_pruned_event() -> Result<()> {
    let f = Fixture::new()?;
    let event = AuditEvent::legacy(
        "independent_audit",
        serde_json::json!({"value": "original"}),
    );
    let prepared = prepare(&f, event.clone(), None)?;
    let state = f.state()?;
    f.db.transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER audit_cannot_update_state BEFORE UPDATE ON agent_states
             BEGIN SELECT RAISE(ABORT,'Audit rewrote State'); END;",
        )?;
        Ok(())
    })?;
    let (value, receipt) = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Committed { value, receipt } => (value, receipt),
        _ => panic!("expected original Audit commit"),
    };
    assert!(value.applied);
    assert!(value.effects.agent_state.is_none());
    assert!(value.effects.input_evidence_delta.records.is_empty());
    assert!(value.effects.input_evidence_delta.query_advance.is_some());
    assert_eq!(f.state()?, state);
    let actual = &value.effects.audit_events[0];
    assert_eq!(actual.id, event.id);
    assert!(actual.event_seq > 0);
    assert!(!actual.event_log_epoch.is_empty());
    assert_eq!(receipt.outcome.scopes[0].audit_events, vec![actual.clone()]);
    let ack = value
        .effects
        .current_read_writes
        .as_ref()
        .context("original Audit Current ACK")?;
    assert!(ack.is_complete());
    let key = (CurrentFamily::Event, event.id.clone());
    let selected = ack
        .record(&key)
        .context("owning original Audit body")?
        .clone();
    let CurrentRecord::Event(body) = selected.record() else {
        panic!("expected Event");
    };
    assert_eq!(body, actual);
    let material = selected.weak_material();
    drop(value);
    assert!(material.upgrade().is_some());
    f.db.transaction(|tx| {
        tx.execute(
            "DELETE FROM audit_events WHERE audit_event_id=?1",
            [&event.id],
        )?;
        Ok(())
    })?;
    match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepared)?
    {
        InputMutationExecution::Replayed {
            receipt: replay, ..
        } => assert_eq!(replay, receipt),
        _ => panic!("expected original Audit receipt replay"),
    }
    assert!(!f.db.audit_events().has_event_by_id(&event.id)?);
    assert_eq!(f.state()?, state);
    let mut different = event.clone();
    different.kind = "different_audit".into();
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare(&f, different, None)?)
        .is_err());
    drop(selected);
    assert!(material.upgrade().is_none());
    Ok(())
}

#[test]
fn independent_audit_faults_roll_back_event_qualification_and_receipt_not_prior_state() -> Result<()>
{
    for fault in [
        TransitionFaultPoint::AfterValidation,
        TransitionFaultPoint::AfterAuditWrites,
        TransitionFaultPoint::BeforeCommit,
    ] {
        let f = Fixture::new()?;
        let mut state = f.state()?;
        state.total_model_rounds += 1;
        f.db.transaction(|tx| upsert_agent_state_tx(tx, &state))?;
        let event = AuditEvent::legacy("independent_audit_fault", serde_json::json!({}));
        let prepared = prepare(&f, event.clone(), Some(fault))?;
        let error =
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&prepared)
                .unwrap_err();
        assert!(
            format!("{error:#}")
                .contains(&format!("injected runtime transition fault at {fault:?}")),
            "{error:#}"
        );
        assert_eq!(f.state()?, state);
        assert!(!f.db.audit_events().has_event_by_id(&event.id)?);
        assert_eq!(f.receipt_count()?, 0);
        let qualified: usize = f.db.connection()?.query_row(
            "SELECT COUNT(*) FROM current_read_qualification WHERE record_id=?1",
            [&event.id],
            |row| row.get(0),
        )?;
        assert_eq!(qualified, 0);
        let retry = prepare(&f, event, None)?;
        assert_eq!(prepared.descriptor(), retry.descriptor());
        assert!(matches!(
            f.db.transitions_in_scope(&f.scope)
                .commit_prepared_transition(&retry)?,
            InputMutationExecution::Committed { .. }
        ));
    }
    Ok(())
}

#[test]
fn independent_audit_does_not_adopt_raw_event_or_deleting_scope() -> Result<()> {
    let f = Fixture::new()?;
    let event = AuditEvent::legacy("independent_audit_raw", serde_json::json!({}));
    f.db.audit_events()
        .append(Some(&f.scope.agent_id), &event)?;
    let value = match f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare(&f, event.clone(), None)?)?
    {
        InputMutationExecution::Committed { value, .. } => value,
        _ => panic!("expected original raw-address observation"),
    };
    let ack = value
        .effects
        .current_read_writes
        .as_ref()
        .context("Current outcome")?;
    let key = (CurrentFamily::Event, event.id);
    assert_eq!(ack.facts().get(&key), Some(&CurrentAddressFact::Unprepared));
    assert!(ack.record(&key).is_none());
    assert!(!ack.is_complete());
    let receipts = f.receipt_count()?;
    let state = f.state()?;
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status='deleting',
             payload_json=json_set(payload_json,'$.status','deleting') WHERE agent_id=?1",
            [&f.scope.agent_id],
        )?;
        Ok(())
    })?;
    let new = AuditEvent::legacy("independent_audit_deleting", serde_json::json!({}));
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare(&f, new.clone(), None)?)
        .is_err());
    assert!(!f.db.audit_events().has_event_by_id(&new.id)?);
    assert_eq!(f.receipt_count()?, receipts);
    assert_eq!(f.state()?, state);
    Ok(())
}

#[test]
fn retiring_audit_observation_is_scope_checked_raw_and_never_a_command() -> Result<()> {
    let f = Fixture::new()?;
    let event = AuditEvent::legacy("retiring_observation", serde_json::json!({}));
    let receipts = f.receipt_count()?;
    let state = f.state()?;
    assert!(f
        .db
        .audit_events()
        .append_retiring_observation(&f.scope, &event)?
        .is_none());
    assert!(!f.db.audit_events().has_event_by_id(&event.id)?);
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status='deleting',
             payload_json=json_set(payload_json,'$.status','deleting') WHERE agent_id=?1",
            [&f.scope.agent_id],
        )?;
        Ok(())
    })?;
    let mut foreign = f.scope.clone();
    foreign.identity_incarnation += 1;
    assert!(f
        .db
        .audit_events()
        .append_retiring_observation(&foreign, &event)
        .is_err());
    assert!(!f.db.audit_events().has_event_by_id(&event.id)?);
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status='active' WHERE agent_id=?1",
            [&f.scope.agent_id],
        )?;
        Ok(())
    })?;
    assert!(f
        .db
        .audit_events()
        .append_retiring_observation(&f.scope, &event)
        .is_err());
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status='deleting' WHERE agent_id=?1",
            [&f.scope.agent_id],
        )?;
        Ok(())
    })?;
    let observed =
        f.db.audit_events()
            .append_retiring_observation(&f.scope, &event)?
            .context("retiring raw observation")?;
    assert_eq!(observed.id, event.id);
    assert!(observed.event_seq > 0);
    assert!(!f.db.connection()?.query_row(
        "SELECT EXISTS(SELECT 1 FROM current_read_qualification
         WHERE agent_id=?1 AND identity_incarnation=?2 AND family='event' AND record_id=?3)",
        rusqlite::params![f.scope.agent_id, f.scope.identity_incarnation, event.id],
        |row| row.get::<_, bool>(0),
    )?);
    assert!(f
        .db
        .transitions_in_scope(&f.scope)
        .commit_prepared_transition(&prepare(&f, event, None)?)
        .is_err());
    f.db.transaction(|tx| {
        tx.execute(
            "UPDATE agent_identities SET status='deleted',
             payload_json=json_set(payload_json,'$.status','deleted') WHERE agent_id=?1",
            [&f.scope.agent_id],
        )?;
        Ok(())
    })?;
    let rejected = AuditEvent::legacy("retired_observation", serde_json::json!({}));
    assert!(f
        .db
        .audit_events()
        .append_retiring_observation(&f.scope, &rejected)
        .is_err());
    assert!(!f.db.audit_events().has_event_by_id(&rejected.id)?);
    assert_eq!(f.receipt_count()?, receipts);
    assert_eq!(f.state()?, state);
    Ok(())
}

#[test]
fn independent_audit_material_refusal_precedes_business_and_receipt() -> Result<()> {
    use crate::runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger, resident_task::TaskCaptureRequest,
    };
    let f = Fixture::new()?;
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits {
        record_limit: 1,
        ..Default::default()
    })?;
    let event = AuditEvent::legacy("independent_audit_denied", serde_json::json!({}));
    let work = crate::runtime_db::work_snapshot::WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 1,
        byte_limit: 64 * 1024 * 1024,
    };
    let tasks = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: work.record_limit,
        byte_limit: work.byte_limit,
    };
    let error =
        f.db.transitions_in_scope(&f.scope)
            .commit_prepared_transition_with_input_query(
                &prepare(&f, event.clone(), None)?,
                &work,
                &ledger,
                &tasks,
                None,
                None,
            )
            .unwrap_err();
    assert!(
        format!("{error:#}").contains("Input read material"),
        "{error:#}"
    );
    assert!(!f.db.audit_events().has_event_by_id(&event.id)?);
    assert_eq!(f.receipt_count()?, 0);
    Ok(())
}
