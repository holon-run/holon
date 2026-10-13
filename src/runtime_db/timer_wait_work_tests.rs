use super::*;
use crate::{
    runtime::resident_work::{
        CanonicalWorkRecord, ResidentWorkDomain, WorkCaptureRequest, WorkFamily, WorkKey,
    },
    runtime_db::{
        repositories::{insert_new_work_item_tx, upsert_wait_condition_with_writes_tx},
        transitions::TransitionFaultPoint,
        work_snapshot::{
            prepare_work_domain_snapshot_tx, prepare_work_write_tx, register_work_writes_tx,
            WorkDomainLoadRequest,
        },
    },
    types::{WaitConditionRecord, WaitConditionStatus, WorkItemRecord, WorkItemState},
};
use std::collections::BTreeSet;

pub(super) fn budget_ledger(
) -> Result<crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger> {
    Ok(crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?)
}

pub(super) fn work_request() -> WorkDomainLoadRequest {
    WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        record_limit: 128,
        byte_limit: 1024 * 1024,
    }
}

fn timer_wait_fixture(
    fixture: &Fixture,
    command: &mut crate::runtime_db::TimerFire,
    qualify_wait: bool,
) -> Result<(WaitConditionRecord, ResidentWorkDomain)> {
    let mut work = WorkItemRecord::new(&fixture.scope.agent_id, "timer owner", WorkItemState::Open);
    work.id = format!("work-{}", command.expected.id);
    command.message.work_item_id = Some(work.id.clone());
    fixture.db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Work, &work.id)]);
        let proof = prepare_work_write_tx(tx, &fixture.scope, &keys)?;
        insert_new_work_item_tx(tx, &work)?;
        register_work_writes_tx(
            tx,
            &proof,
            &keys,
            &BTreeSet::new(),
            &command.agent_state.0,
            &work_request(),
        )?;
        Ok(())
    })?;
    let wait: WaitConditionRecord = serde_json::from_value(serde_json::json!({
        "id":format!("wait-{}", command.expected.id),
        "agent_id":fixture.scope.agent_id,
        "work_item_id":work.id,
        "status":"active", "kind":"timer", "source":"WaitFor",
        "subject_ref":command.expected.id,
        "waiting_for":"the timer wake",
        "wake_sources":[{"kind":"timer","wake_at":command.expected.next_fire_at.unwrap()}],
        "created_at":command.expected.created_at,
        "updated_at":command.expected.created_at
    }))?;
    fixture.db.transaction(|tx| {
        let keys = BTreeSet::from([WorkKey::new(WorkFamily::Wait, &wait.id)]);
        let proof = qualify_wait
            .then(|| prepare_work_write_tx(tx, &fixture.scope, &keys))
            .transpose()?;
        let writes = upsert_wait_condition_with_writes_tx(tx, &wait)?;
        assert_eq!(writes.wait_ids, BTreeSet::from([wait.id.clone()]));
        if let Some(proof) = proof {
            register_work_writes_tx(
                tx,
                &proof,
                &keys,
                &BTreeSet::new(),
                &command.agent_state.0,
                &work_request(),
            )?;
        }
        Ok(())
    })?;
    let before = fixture.db.transaction(|tx| {
        Ok(
            prepare_work_domain_snapshot_tx(tx, &fixture.scope, &work_request())?
                .require_current()?
                .resident,
        )
    })?;
    Ok((wait, before))
}

fn observed_wait_status(domain: &ResidentWorkDomain, wait_id: &str) -> Result<WaitConditionStatus> {
    let captured = domain.capture(&WorkCaptureRequest::current())?;
    let record = captured
        .lookup(&WorkKey::new(WorkFamily::Wait, wait_id))?
        .unwrap();
    let CanonicalWorkRecord::Wait(wait) = &record.record else {
        panic!("expected Wait ACK")
    };
    Ok(wait.status.clone())
}

#[test]
fn scoped_timer_fire_publishes_triggered_wait_in_original_work_ack() -> Result<()> {
    let f = Fixture::new()?;
    let mut command = timer_fire_fixture(&f, "work-timer-wake")?;
    let (wait, before) = timer_wait_fixture(&f, &mut command, true)?;
    assert_eq!(
        observed_wait_status(&before, &wait.id)?,
        WaitConditionStatus::Active
    );
    let ack = f.db.timers().fire_with_wait_in_scope(
        &f.scope,
        &command,
        &work_request(),
        &budget_ledger()?,
        None,
    )?;
    let delta = ack
        .work_delta
        .as_ref()
        .expect("actual triggered Wait needs a Work delta");
    assert_eq!(delta.base_revision, before.coverage().revision);
    assert_eq!(delta.scope, f.scope);
    let installed = before.with_committed(delta)?;
    assert_eq!(
        observed_wait_status(&installed, &wait.id)?,
        WaitConditionStatus::Triggered
    );
    assert_eq!(
        observed_wait_status(&before, &wait.id)?,
        WaitConditionStatus::Active
    );
    let fresh = f.db.transaction(|tx| {
        Ok(
            prepare_work_domain_snapshot_tx(tx, &f.scope, &work_request())?
                .require_current()?
                .resident,
        )
    })?;
    assert_eq!(fresh.coverage(), installed.coverage());
    assert_eq!(
        observed_wait_status(&fresh, &wait.id)?,
        WaitConditionStatus::Triggered
    );
    assert_eq!(ack.record.message.as_ref().unwrap().message_seq, Some(1));
    assert_eq!(
        ack.agent_state.as_ref().unwrap().record.total_message_count,
        1
    );
    let replay = f.db.timers().fire_with_wait_in_scope(
        &f.scope,
        &command,
        &work_request(),
        &budget_ledger()?,
        None,
    )?;
    assert!(!replay.record.advanced);
    assert!(replay.work_delta.is_none());
    Ok(())
}

#[test]
fn scoped_timer_work_finalization_failure_rolls_back_wait_message_and_timer() -> Result<()> {
    let f = Fixture::new()?;
    let mut command = timer_fire_fixture(&f, "rollback-work-timer-wake")?;
    let (wait, before) = timer_wait_fixture(&f, &mut command, true)?;
    assert!(f
        .db
        .timers()
        .fire_with_wait_in_scope(
            &f.scope,
            &command,
            &work_request(),
            &budget_ledger()?,
            Some(TransitionFaultPoint::AfterCanonicalWrites),
        )
        .is_err());
    assert_eq!(
        f.db.timers().latest(&command.expected.id)?,
        Some(command.expected.clone())
    );
    assert!(f.db.timers().pending_wake(&command.expected.id)?.is_none());
    assert!(f.db.queue_entries().latest(&command.message.id)?.is_none());
    assert!(f
        .db
        .messages()
        .by_id(Some(&f.scope.agent_id), &command.message.id)?
        .is_none());
    assert_eq!(
        f.db.agent_states().latest(&f.scope.agent_id)?,
        Some(command.agent_state.0.clone())
    );
    assert_eq!(f.aliases()?, 0);
    let fresh = f.db.transaction(|tx| {
        Ok(
            prepare_work_domain_snapshot_tx(tx, &f.scope, &work_request())?
                .require_current()?
                .resident,
        )
    })?;
    assert_eq!(fresh.coverage(), before.coverage());
    assert_eq!(
        observed_wait_status(&fresh, &wait.id)?,
        WaitConditionStatus::Active
    );
    let ack = f.db.timers().fire_with_wait_in_scope(
        &f.scope,
        &command,
        &work_request(),
        &budget_ledger()?,
        None,
    )?;
    assert_eq!(
        observed_wait_status(
            &before.with_committed(ack.work_delta.as_ref().unwrap())?,
            &wait.id
        )?,
        WaitConditionStatus::Triggered
    );
    Ok(())
}

#[test]
fn scoped_timer_fire_refuses_unqualified_wait_instead_of_adopting_it() -> Result<()> {
    let f = Fixture::new()?;
    let mut command = timer_fire_fixture(&f, "unqualified-work-timer-wake")?;
    let (wait, before) = timer_wait_fixture(&f, &mut command, false)?;
    assert!(!before.coverage().unresolved_wait_ids.contains(&wait.id));
    assert!(f
        .db
        .timers()
        .fire_with_wait_in_scope(&f.scope, &command, &work_request(), &budget_ledger()?, None)
        .is_err());
    assert_eq!(
        f.db.timers().latest(&command.expected.id)?,
        Some(command.expected)
    );
    assert!(f.db.timers().pending_wake(&command.record.id)?.is_none());
    assert!(f.db.queue_entries().latest(&command.message.id)?.is_none());
    assert!(f
        .db
        .messages()
        .by_id(Some(&f.scope.agent_id), &command.message.id)?
        .is_none());
    let status: String = f.db.connection()?.query_row(
        "SELECT status FROM wait_conditions WHERE wait_condition_id=?1",
        [&wait.id],
        |row| row.get(0),
    )?;
    assert_eq!(status, "active");
    let aliases: i64 = f.db.connection()?.query_row(
        "SELECT count(*) FROM work_domain_records WHERE agent_id=?1 AND family='wait' AND record_id=?2",
        rusqlite::params![f.scope.agent_id, wait.id], |row|row.get(0),
    )?;
    assert_eq!(aliases, 0);
    Ok(())
}

#[test]
fn scoped_timer_projection_rejects_oversize_before_decode_and_business_writes() -> Result<()> {
    use crate::runtime_db::work_snapshot::WorkSnapshotError;
    let f = Fixture::new()?;
    let mut command = timer_fire_fixture(&f, "projection-budget-timer")?;
    let (wait, before) = timer_wait_fixture(&f, &mut command, true)?;
    let original: String = f.db.connection()?.query_row(
        "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
        [&wait.id],
        |row| row.get(0),
    )?;
    // Valid JSON deliberately lacks typed Wait fields. Budget must win before
    // serde or the first Timer/Wait business write, even for a corrupt raw pool.
    let oversize = serde_json::to_string(&serde_json::json!({"padding":"x".repeat(16 * 1024)}))?;
    f.db.transaction(|tx| {
        tx.execute("UPDATE wait_conditions SET payload_json=?1 WHERE wait_condition_id=?2",
            rusqlite::params![oversize,wait.id])?;
        tx.execute_batch("CREATE TRIGGER reject_timer_before_projection_budget BEFORE UPDATE ON timers BEGIN SELECT RAISE(ABORT,'business write preceded projection budget'); END;")?;
        Ok(())
    })?;
    let ledger = budget_ledger()?;
    let mut request = work_request();
    request.byte_limit = 4096;
    let error =
        f.db.timers()
            .fire_with_wait_in_scope(&f.scope, &command, &request, &ledger, None)
            .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<WorkSnapshotError>(),
            Some(WorkSnapshotError::ResourceExhausted(
                "TriggerWaitAndEnqueue projection"
            ))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    assert_eq!(
        f.db.timers().latest(&command.expected.id)?,
        Some(command.expected.clone())
    );
    assert!(f.db.timers().pending_wake(&command.expected.id)?.is_none());
    assert!(f.db.queue_entries().latest(&command.message.id)?.is_none());
    assert!(f
        .db
        .messages()
        .by_id(Some(&f.scope.agent_id), &command.message.id)?
        .is_none());
    assert_eq!(
        f.db.agent_states().latest(&f.scope.agent_id)?,
        Some(command.agent_state.0.clone())
    );
    // A larger allowance reaches serde, whose error must also release the guard.
    let decode_error =
        f.db.transaction(|tx| {
            crate::runtime_db::transitions::predicted_trigger_wait_tx(
                tx,
                &f.scope,
                &command.message,
                &command.queue_entry,
                &work_request(),
                &ledger,
                None,
            )
        })
        .unwrap_err();
    assert!(
        decode_error.downcast_ref::<serde_json::Error>().is_some(),
        "{decode_error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    f.db.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER reject_timer_before_projection_budget")?;
        tx.execute(
            "UPDATE wait_conditions SET payload_json=?1 WHERE wait_condition_id=?2",
            rusqlite::params![original, wait.id],
        )?;
        Ok(())
    })?;
    let ack = f.db.timers().fire_with_wait_in_scope(
        &f.scope,
        &command,
        &work_request(),
        &ledger,
        None,
    )?;
    assert_eq!(
        observed_wait_status(
            &before.with_committed(ack.work_delta.as_ref().unwrap())?,
            &wait.id
        )?,
        WaitConditionStatus::Triggered
    );
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn trigger_wait_projection_budget_follows_original_work_proof() -> Result<()> {
    use crate::runtime::input_evidence_budget::InputMemoryCategory;
    let f = Fixture::new()?;
    let mut command = timer_fire_fixture(&f, "projection-proof-lifetime")?;
    let (wait, _) = timer_wait_fixture(&f, &mut command, true)?;
    let ledger = budget_ledger()?;
    f.db.transaction(|tx| {
        let (predicted, guard) = crate::runtime_db::transitions::predicted_trigger_wait_tx(
            tx,
            &f.scope,
            &command.message,
            &command.queue_entry,
            &work_request(),
            &ledger,
            None,
        )?;
        let predicted = predicted.unwrap();
        assert_eq!(predicted.id, wait.id);
        let keys = crate::runtime_db::work_snapshot::work_wait_write_keys_tx(tx, &[predicted])?;
        let mut proof = prepare_work_write_tx(tx, &f.scope, &keys)?;
        proof.retain_preparation_budget(guard.into_iter().collect());
        let observed = ledger.report()?;
        assert!(observed.owned.payload_bytes > 0);
        assert!(observed.categories[&InputMemoryCategory::Candidate].bytes > 0);
        drop(proof);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        Ok(())
    })?;
    Ok(())
}
