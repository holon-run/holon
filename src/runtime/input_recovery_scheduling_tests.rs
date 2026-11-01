use super::*;
use crate::{
    runtime::resident_scheduling::{
        CanonicalSchedulingFact, SchedulingCaptureRequest, SchedulingFamily, SchedulingKey,
    },
    runtime_db::{transitions::PreparedTimerTransition, TimerCancel},
    types::{TimerRecord, TimerStatus},
};

#[test]
fn positive_original_receipt_installs_fresh_scheduling_and_never_replays_old_timer_body(
) -> Result<()> {
    let f = Fixture::new()?;
    let at: chrono::DateTime<chrono::Utc> = "2026-10-14T00:00:00.123456789Z".parse()?;
    let timer = TimerRecord {
        calendar: None,
        id: "original-scheduling-timer".into(),
        agent_id: f.scope.agent_id.clone(),
        created_at: at,
        duration_ms: 3600000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(at + chrono::Duration::hours(1)),
        last_fired_at: None,
        fire_count: 0,
    };
    let create = PreparedInputTransition::timer(
        f.scope.clone(),
        "actual-timer-create".into(),
        at,
        PreparedTimerTransition::create(timer.clone(), None),
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&create)?;
    let command = f.prepared(None, false)?;
    let (reservation, witness) = f.begin(&command)?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&command)?;
    witness.finish_original_attempt();
    drop(reservation);
    // A later actual typed ACK is the current domain; the original posture receipt
    // supplies only recovery responsibility, never its old Timer body.
    let cancelled = TimerRecord {
        status: TimerStatus::Cancelled,
        next_fire_at: None,
        ..timer.clone()
    };
    let cancel = PreparedInputTransition::timer(
        f.scope.clone(),
        "actual-timer-cancel".into(),
        at,
        PreparedTimerTransition::cancel(
            TimerCancel {
                expected: timer,
                record: cancelled.clone(),
                agent_state: None,
                cancelled_at: at,
            },
            None,
            None,
        ),
    )?;
    f.db.transitions_in_scope(&f.scope)
        .commit_prepared_transition(&cancel)?;
    let revision:u64=f.db.connection()?.query_row(
        "SELECT revision FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![f.scope.agent_id,f.scope.identity_incarnation],|row|row.get(0))?;
    let mut reservation = f.owner.try_reserve()?;
    assert!(f.resolve(&mut reservation)?);
    let known = MutationReservation::validate_group_known(std::slice::from_ref(&reservation))?;
    let Some(KnownCommit::State {
        scheduling: Some(domain),
        ..
    }) = &known[0]
    else {
        panic!("positive recovery must install current Scheduling from the same fresh TX")
    };
    let captured = domain.capture(&SchedulingCaptureRequest::current())?;
    assert_eq!(captured.revision, revision);
    assert_eq!(captured.active_timer_count()?, 0);
    let row = captured
        .lookup(&SchedulingKey::new(SchedulingFamily::Timer, &cancelled.id))?
        .unwrap();
    assert_eq!(row.fact, CanonicalSchedulingFact::Timer(cancelled));
    MutationReservation::reconcile_all(std::slice::from_mut(&mut reservation), known)?;
    let root = f.owner.capture()?.unwrap();
    assert_eq!(
        root.scheduling.as_ref().unwrap().coverage().revision,
        revision
    );
    let after:u64=f.db.connection()?.query_row(
        "SELECT revision FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![f.scope.agent_id,f.scope.identity_incarnation],|row|row.get(0))?;
    assert_eq!(revision, after);
    Ok(())
}
