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
async fn context_plan_moves_its_capture_without_second_admission_or_retaining_domain() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.schedule_timer(3_600_000, None, None).await?;
    let owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let source = root.scheduling.as_ref().unwrap();
    let holders = Arc::strong_count(source);
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new(
        &runtime.inner.input_evidence_budget,
        source.scope(),
    )?;
    let _entered = material.enter();
    let plan = owner.capture_context_scheduling_plan()?;
    assert_eq!(Arc::strong_count(source), holders + 1);
    let records = plan.read.scheduling.records.as_ptr();
    let before = material.finish()?;
    let captured = plan.into_current_capture(&root.state, source)?;
    assert_eq!(captured.records.as_ptr(), records);
    assert_eq!(material.finish()?, before);
    assert_eq!(Arc::strong_count(source), holders);

    let plan = owner.capture_context_scheduling_plan()?;
    let previous_records = plan.read.scheduling.records.as_ptr();
    let mut coverage = source.coverage().clone();
    let previous_revision = coverage.revision;
    coverage.revision += 1;
    let changed = Arc::new(
        source.with_committed(
            &crate::runtime::resident_scheduling::CommittedSchedulingDelta {
                scope: source.scope().clone(),
                base_revision: previous_revision,
                revision: coverage.revision,
                coverage,
                records: Arc::new(
                    plan.read
                        .scheduling
                        .records
                        .iter()
                        .map(|record| (record.locator.key.clone(), record.clone()))
                        .collect(),
                ),
            },
        )?,
    );
    let before = material.finish()?;
    let refreshed = plan.into_current_capture(&root.state, &changed)?;
    assert_eq!(refreshed.revision, previous_revision + 1);
    assert_ne!(refreshed.records.as_ptr(), previous_records);
    assert!(material.finish()?.1 > before.1);

    let plan = owner.capture_context_scheduling_plan()?;
    let mut changed_state = root.state.as_ref().clone();
    changed_state.id.push_str("-foreign");
    assert!(plan
        .into_current_capture(&Arc::new(changed_state), source)
        .unwrap_err()
        .to_string()
        .contains("State changed"));
    Ok(())
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

// A related deletion scope publishes metadata, not a new Scheduling ACK.
async fn publish_active_metadata_ack(runtime: &RuntimeHandle) -> Result<()> {
    use crate::runtime::mutation_owner::{
        IdentityMutationFacts, KnownCommit, MutationOwner, MutationReservation,
    };
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let identity = runtime
        .inner
        .runtime_db
        .agent_identities()
        .latest(&root.state.id)?
        .unwrap();
    let mut owners = MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
    MutationReservation::begin_all(&mut owners, crate::ids::audit_event_id())?;
    let known = KnownCommit::Metadata(Arc::new(IdentityMutationFacts {
        identity: Some(Arc::new(identity)),
        state: Some(root.state.clone()),
    }));
    MutationReservation::committed_all(&mut owners, vec![known.clone()])?;
    runtime.install_saved_mutation(&owners[0], &known).await?;
    MutationReservation::reconcile_all(&mut owners, vec![Some(known)])?;
    drop(owners);
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(after.state, root.state);
    assert!(after.scheduling.is_none() && after.current.is_none());
    assert!(after.work.is_some() && after.tasks.is_some());
    let error = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await
        .err()
        .unwrap();
    assert!(matches!(
        error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
        Some(crate::runtime::resident_scheduling::SchedulingReadError::IncompleteLive)
    ));
    Ok(())
}

#[tokio::test]
async fn summary_prepares_invalidated_scheduling_after_active_metadata_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    publish_active_metadata_ack(&runtime).await?;
    let summary = runtime.agent_summary().await?;
    assert_eq!(summary.agent.id, "scheduling-reader");
    let read = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    assert_eq!(read.scheduling.timer(&timer.id)?, Some(&timer));
    assert_eq!(read.scheduling.active_timer_count()?, 1);
    Ok(())
}

#[tokio::test]
async fn work_tool_result_prepares_scheduling_after_active_metadata_ack_without_repeating_write(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let work = runtime
        .create_work_item("before metadata".into(), None, None, Vec::new())
        .await?;
    publish_active_metadata_ack(&runtime).await?;
    let result = crate::tool::tools::update_work_item::execute(
        &runtime,
        "scheduling-reader",
        &crate::types::AuthorityClass::OperatorInstruction,
        &serde_json::json!({"work_item_id":work.id,"objective":"one original update"}),
    )
    .await;
    let stored = runtime.validate_owned_work_item("scheduling-reader", &work.id)?;
    assert_eq!(stored.objective, "one original update");
    assert_eq!(stored.revision, work.revision + 1);
    let result = result?;
    assert!(!result.is_error());

    Ok(())
}

#[tokio::test]
async fn compact_state_prepares_scheduling_after_active_metadata_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    publish_active_metadata_ack(&runtime).await?;
    let summary = runtime.lightweight_agent_state_projection().await?;
    assert_eq!(summary.agent.id, "scheduling-reader");
    assert!(runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .scheduling
        .is_some());
    Ok(())
}

#[tokio::test]
async fn readonly_scoped_roster_prepares_scheduling_after_active_metadata_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    publish_active_metadata_ack(&runtime).await?;
    let identity = runtime
        .inner
        .runtime_db
        .agent_identities()
        .latest("scheduling-reader")?
        .unwrap();
    let scope = crate::runtime_db::RuntimeDbReadScope::new(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    );
    let db = runtime.inner.runtime_db.with_read_scope(scope);
    let material = crate::diagnostics::resource_io::ResourceReadMaterial::new(
        &runtime.inner.input_evidence_budget,
        &crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: "scheduling-reader".into(),
            identity_incarnation: identity.incarnation,
        },
    )?;
    // The pool enforces query_only: this reconstruction cannot write sidecars.
    let entry = runtime
        .agent_list_entry_read(&identity, &material, &db)
        .await?;
    assert_eq!(entry.identity.agent_id, "scheduling-reader");
    let read = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    assert_eq!(read.scheduling.timer(&timer.id)?, Some(&timer));
    Ok(())
}

#[tokio::test]
async fn summary_query_after_active_metadata_ack_rejects_unqualified_canonical_drift() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    publish_active_metadata_ack(&runtime).await?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let mut changed = timer.clone();
    changed.summary = Some("unowned canonical drift".into());
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute(
            "UPDATE timers SET payload_json=?2 WHERE timer_id=?1",
            rusqlite::params![timer.id, serde_json::to_string(&changed)?],
        )?;
        Ok(())
    })?;
    let error = runtime.agent_summary().await.err().unwrap();
    eprintln!("original Scheduling drift rejected: {error:#}");
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(after.token, before.token);
    assert!(after.scheduling.is_none() && after.current.is_none());
    // Restore only the deliberately changed fixture bytes. The exact same
    // accepted query must now succeed, without adopting the corrupt body.
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute(
            "UPDATE timers SET payload_json=?2 WHERE timer_id=?1",
            rusqlite::params![timer.id, serde_json::to_string(&timer)?],
        )?;
        Ok(())
    })?;
    runtime.agent_summary().await?;
    let read = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    assert_eq!(read.scheduling.timer(&timer.id)?, Some(&timer));
    Ok(())
}
