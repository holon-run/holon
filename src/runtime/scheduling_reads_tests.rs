use super::*;
use crate::runtime::{
    mutation_owner::MutationOwnerError,
    resident_scheduling::{SchedulingCaptureRequest, SchedulingFamily, SchedulingKey},
    tests::support::{context_config, tempdir, StubProvider},
};
use crate::runtime_db::transitions::TransitionFaultPoint;
use anyhow::Context;

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "scheduling-reader",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "scheduling-reader".into(),
        context_config(),
    )
}

#[tokio::test]
async fn scheduling_bootstrap_and_ack_publish_one_root_and_capture_never_queries_timer_tables(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let first = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(first.scheduling.is_some());
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("same-root".into()))
        .await?;
    let owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let captured = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    assert_eq!(captured.token, root.token);
    assert_eq!(captured.state, root.state);
    assert_eq!(captured.scheduling.timer(&timer.id)?, Some(&timer));
    assert_eq!(captured.scheduling.active_timer_count()?, 1);
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO timers_read_forbidden; ALTER TABLE timer_wakes RENAME TO timer_wakes_read_forbidden; ALTER TABLE external_triggers RENAME TO external_triggers_read_forbidden")?;
        Ok(())
    })?;
    let pure = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    assert_eq!(pure.token, captured.token);
    assert_eq!(pure.scheduling.timer(&timer.id)?, Some(&timer));
    assert!(pure.scheduling.pending_wake(&timer.id)?.is_none());
    assert!(pure.scheduling.active_default_trigger()?.is_none());
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE external_triggers_read_forbidden RENAME TO external_triggers; ALTER TABLE timer_wakes_read_forbidden RENAME TO timer_wakes; ALTER TABLE timers_read_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    drop(owner);
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn scheduling_known_fault_reconciles_actual_cancelled_body_without_repeating_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    let old = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
    let error = runtime.cancel_timer(&timer.id).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("publication requires reconciliation"),
        "{error:#}"
    );
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(_))
    ));
    let revision:u64=runtime.inner.runtime_db.connection()?.query_row(
        "SELECT revision FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![old.scheduling.scope.agent_id,old.scheduling.scope.identity_incarnation],|row|row.get(0))?;
    let fresh = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    assert_eq!(fresh.scheduling.revision, revision);
    assert_eq!(fresh.scheduling.active_timer_count()?, 0);
    assert_eq!(
        fresh.scheduling.timer(&timer.id)?.unwrap().status,
        crate::types::TimerStatus::Cancelled
    );
    assert_eq!(
        old.scheduling.timer(&timer.id)?.unwrap().status,
        crate::types::TimerStatus::Active
    );
    let after:u64=runtime.inner.runtime_db.connection()?.query_row(
        "SELECT revision FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
        rusqlite::params![fresh.scheduling.scope.agent_id,fresh.scheduling.scope.identity_incarnation],|row|row.get(0))?;
    assert_eq!(revision, after);
    assert!(fresh.token.generation > old.token.generation);
    Ok(())
}

#[tokio::test]
async fn scheduling_missing_domain_requires_named_owner_preparation_and_unknown_key_stays_typed(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    reservation.begin(crate::ids::audit_event_id())?;
    reservation.committed_prepared_with_scheduling_domains(
        root.state.clone(),
        root.episodes.clone(),
        root.input_evidence.clone(),
        root.work.clone(),
        root.tasks.clone(),
        None,
    )?;
    reservation.publish(root.state.clone())?;
    drop(reservation);
    let mut owner = runtime.begin_mutation().await?;
    assert!(owner
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .is_err());
    owner
        .prepare_owner_scheduling_request(&SchedulingCaptureRequest::current())
        .await?;
    let first = owner.capture_published_root()?;
    owner
        .prepare_owner_scheduling_request(&SchedulingCaptureRequest::current())
        .await?;
    assert_eq!(first.token, owner.capture_published_root()?.token);
    let selected = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    assert!(selected
        .scheduling
        .lookup(&SchedulingKey::new(
            SchedulingFamily::Timer,
            "unrequested-late-key"
        ))
        .is_err());
    assert_eq!(selected.scheduling.active_timer_count()?, 0);
    Ok(())
}

#[tokio::test]
async fn same_root_scheduler_and_work_planning_do_not_read_timer_or_trigger_bodies() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    let mut owner = runtime.begin_mutation().await?;
    owner
        .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
        .await?;
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO planner_timers_forbidden; ALTER TABLE timer_wakes RENAME TO planner_wakes_forbidden; ALTER TABLE external_triggers RENAME TO planner_triggers_forbidden")?;
        Ok(())
    })?;
    let pure = owner.capture_scheduler_read(
        &crate::runtime::resident_work::WorkCaptureRequest::current(),
        runtime.now(),
    )?;
    assert_eq!(pure.state.id, "scheduling-reader");
    let scheduling = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    assert_eq!(scheduling.scheduling.active_timer_count()?, 1);
    drop(owner);
    let work = runtime.capture_work_query_snapshot().await?;
    assert_eq!(work.state, scheduling.state);
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE planner_triggers_forbidden RENAME TO external_triggers; ALTER TABLE planner_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE planner_timers_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn scheduling_selected_and_trigger_output_use_one_allowance_before_clone() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    let read = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    let (selected, records) = read.scheduling.selected_material_estimate()?;
    // Resource refresh changes the runtime ledger concurrently with this output-only check.
    let ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        runtime.inner.input_evidence_budget.report()?.limits,
    )?;
    let baseline = ledger.report()?.observed_bytes;
    let error = read
        .scheduling
        .clone_trigger_records(&ledger, 65536, usize::try_from(selected + 2047)?)
        .err()
        .context("combined byte limit must reject before clone")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
        Some(crate::runtime::resident_scheduling::SchedulingReadError::ResourceExhausted(_))
    ));
    assert!(read
        .scheduling
        .clone_trigger_records(&ledger, records + 3, 64 * 1024 * 1024)
        .is_err());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let owned = read.scheduling.clone_trigger_records(
        &ledger,
        records + 4,
        usize::try_from(selected + 2048)?,
    )?;
    assert!(owned.records.is_empty());
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(owned);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}
