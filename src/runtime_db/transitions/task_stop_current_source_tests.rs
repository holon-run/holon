//! Physical-only phases keep an original Current source; no implicit body read.
use super::*;
use crate::runtime::current_read_facts::{CurrentReadError, CurrentReadRequest};
use crate::runtime_db::current_read_snapshot as reads;

fn current_request() -> CurrentReadRequest {
    CurrentReadRequest::current(64 * 1024 * 1024, 65536)
}
fn seed_queue(f: &Fixture, l: &InputEvidenceBudgetLedger) -> Result<()> {
    let p = PreparedInputTransition::queue(
        f.scope.clone(),
        "physical-existing-current-queue".into(),
        f.at,
        f.queue()?,
    )?;
    commit(f, &p, l)?;
    Ok(())
}
fn current(f: &Fixture, l: &InputEvidenceBudgetLedger) -> Result<reads::CurrentReadSnapshot> {
    let state = f.state()?;
    f.db.transaction(|tx| {
        reads::prepare_current_read_snapshot_tx(tx, &f.scope, &state, &current_request(), l)
    })
}
fn attention(
    f: &Fixture,
    stop: &StopFixture,
    l: &InputEvidenceBudgetLedger,
) -> Result<PreparedInputTransition> {
    PreparedInputTransition::task_stop_safe_progress(
        &stop.record,
        "physical-original-current-source".into(),
        stop.record.updated_at + chrono::Duration::seconds(1),
        stops::TaskStopProgressAction::PhysicalAttention {
            reason: "no exit proof; preserve original responsibility".into(),
        },
        &request(),
        l,
    )
}

#[test]
fn physical_progress_uses_original_current_arcs_without_queue_or_event_body_sql() -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    seed_queue(&f, &l)?;
    let stop = original_stop(&f, &l)?;
    let old = current(&f, &l)?;
    let selected = old.resident.capture(&current_request())?;
    assert!(selected.has_queued() && !selected.records.is_empty());
    let source = Arc::downgrade(&selected.records[0]);
    let p = attention(&f, &stop, &l)?;
    let descriptor = p.descriptor().clone();
    let p = p.with_current_read_source(old.resident.clone())?;
    assert_eq!(
        p.descriptor(),
        &descriptor,
        "read witness never changes original business address"
    );
    let state = f.state()?;
    f.db.connection()?.execute_batch("ALTER TABLE queue_entries RENAME TO physical_queue_forbidden; ALTER TABLE audit_events RENAME TO physical_audit_forbidden;")?;
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
        unreachable!()
    };
    assert_eq!(
        ack(&receipt).progress.physical,
        stops::TaskStopPhysicalState::NeedsAttention
    );
    let facts = value
        .effects
        .current_read_facts
        .as_ref()
        .context("actual Current source ACK missing")?;
    assert_eq!(
        (facts.base_revision, facts.revision),
        (
            old.resident.coverage().revision,
            old.resident.coverage().revision
        )
    );
    assert!(value.effects.current_read_maintenance.is_none());
    assert!(value.effects.agent_state.is_none() && value.effects.audit_events.is_empty());
    assert!(value.effects.task_delta.is_some());
    assert_eq!(f.state()?, state);
    let pure = facts.snapshot.resident.capture(&current_request())?;
    for prior in &selected.records {
        let after = pure
            .records
            .iter()
            .find(|r| r.locator.key == prior.locator.key)
            .unwrap();
        assert!(Arc::ptr_eq(prior, after));
    }
    let version: u64 = f.db.connection()?.query_row(
        "SELECT revision FROM current_read_current WHERE agent_id=?1",
        [&f.scope.agent_id],
        |r| r.get(0),
    )?;
    let InputMutationExecution::Replayed {
        receipt: replay, ..
    } = commit(&f, &p, &l)?
    else {
        unreachable!()
    };
    assert_eq!(replay, receipt);
    assert_eq!(version, old.resident.coverage().revision);
    drop((pure, selected, value, receipt, replay, p, old, stop));
    assert!(source.upgrade().is_none());
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn stale_physical_phase_state_source_rejects_before_stop_sql_and_fresh_witness_keeps_address(
) -> Result<()> {
    let f = Fixture::new()?;
    let l = ledger()?;
    seed_queue(&f, &l)?;
    let stop = original_stop(&f, &l)?;
    let old = current(&f, &l)?;
    let p = attention(&f, &stop, &l)?.with_current_read_source(old.resident.clone())?;
    let descriptor = p.descriptor().clone();
    let mut after = f.state()?;
    after.last_wake_reason = Some("lawful-new-physical-phase-state".into());
    let posture = PreparedInputTransition::posture(
        f.scope.clone(),
        "lawful-new-state-source".into(),
        f.at + chrono::Duration::seconds(3),
        f.posture(after.clone(), None)?,
        None,
    )?
    .with_current_read_source(old.resident.clone())?;
    let InputMutationExecution::Committed {
        value: state_ack, ..
    } = commit(&f, &posture, &l)?
    else {
        unreachable!()
    };
    let fresh = state_ack
        .effects
        .current_read_facts
        .as_ref()
        .unwrap()
        .snapshot
        .resident
        .clone();
    // Message count is synchronized by the original epilogue. Use a real
    // persisted State field and prove this ACK actually changed the source.
    assert_eq!(f.state()?, after);
    assert_ne!(
        fresh.coverage().state_digest,
        old.resident.coverage().state_digest
    );
    f.db.transaction(|tx| {
        assert_eq!(
            stops::observe_task_stop_phase_mode_tx(tx, &f.scope)?,
            stops::TaskStopPhaseInstallMode::Current
        );
        Ok(())
    })?;
    let before = rows(&f)?;
    let held = l.report()?.observed_bytes;
    let error = commit(&f, &p, &l)
        .err()
        .context("stale State source accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::CanonicalChanged)
        ),
        "{error:#}"
    );
    assert_eq!(rows(&f)?, before);
    assert_eq!(l.report()?.observed_bytes, held);
    let retry = attention(&f, &stop, &l)?.with_current_read_source(fresh.clone())?;
    assert_eq!(retry.descriptor(), &descriptor);
    let InputMutationExecution::Committed { value, receipt } = commit(&f, &retry, &l)? else {
        unreachable!()
    };
    assert_eq!(
        ack(&receipt).progress.physical,
        stops::TaskStopPhysicalState::NeedsAttention
    );
    assert_eq!(
        value.effects.current_read_facts.as_ref().unwrap().revision,
        fresh.coverage().revision
    );
    assert_eq!(f.state()?, after);
    drop((
        value, receipt, retry, fresh, state_ack, posture, p, old, stop,
    ));
    assert_eq!(l.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn original_current_witness_never_turns_no_state_or_poisoned_successor_into_ready() -> Result<()> {
    for retired in [false, true] {
        let f = Fixture::new()?;
        let l = ledger()?;
        seed_queue(&f, &l)?;
        let stop = original_stop(&f, &l)?;
        let old = current(&f, &l)?;
        let p = attention(&f, &stop, &l)?.with_current_read_source(old.resident.clone())?;
        if retired {
            f.db.connection()?.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&f.scope.agent_id])?;
            f.db.connection()?.execute("UPDATE agent_states SET payload_json='poisoned-successor-current-source-state' WHERE agent_id=?1",[&f.scope.agent_id])?;
        } else {
            f.db.connection()?.execute(
                "DELETE FROM agent_states WHERE agent_id=?1",
                [&f.scope.agent_id],
            )?;
        }
        let before: (u64,u64)=f.db.connection()?.query_row(
            "SELECT (SELECT revision FROM current_read_current),(SELECT COUNT(*) FROM current_read_members)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let InputMutationExecution::Committed { value, receipt } = commit(&f, &p, &l)? else {
            unreachable!()
        };
        assert_eq!(
            ack(&receipt).install_mode,
            if retired {
                stops::TaskStopPhaseInstallMode::Retired
            } else {
                stops::TaskStopPhaseInstallMode::NoState
            }
        );
        assert!(
            value.effects.current_read_facts.is_none()
                && value.effects.current_read_maintenance.is_none()
                && value.effects.agent_state.is_none()
                && value.effects.task_delta.is_none()
        );
        let after:(u64,u64)=f.db.connection()?.query_row(
            "SELECT (SELECT revision FROM current_read_current),(SELECT COUNT(*) FROM current_read_members)",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
        assert_eq!(after, before);
        if retired {
            let raw: String = f.db.connection()?.query_row(
                "SELECT payload_json FROM agent_states WHERE agent_id=?1",
                [&f.scope.agent_id],
                |r| r.get(0),
            )?;
            assert_eq!(raw, "poisoned-successor-current-source-state");
        }
        drop((value, receipt, p, old, stop));
        assert_eq!(l.report()?.observed_bytes, 0);
    }
    Ok(())
}
