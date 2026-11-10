use super::*;
use crate::runtime::{
    mutation_owner::MutationOwnerError,
    resident_scheduling::{SchedulingCaptureRequest, SchedulingFamily, SchedulingKey},
    tests::support::{context_config, tempdir, StubProvider},
};
use crate::runtime_db::transitions::TransitionFaultPoint;

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
    let published = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(published.scheduling.is_none());
    let observed = runtime.capture_observer_root(&runtime.inner.runtime_db)?;
    let scheduling = observed
        .scheduling
        .as_ref()
        .unwrap()
        .capture(&SchedulingCaptureRequest::current())?;
    assert_eq!(scheduling.timer(&timer.id)?, Some(&timer));
    assert_eq!(scheduling.active_timer_count()?, 1);
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
        .is_none());
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
    assert!(runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .scheduling
        .is_none());
    let root = runtime.capture_observer_root(&runtime.inner.runtime_db)?;
    let selected = root
        .scheduling
        .as_ref()
        .unwrap()
        .capture(&SchedulingCaptureRequest::current())?;
    assert_eq!(selected.timer(&timer.id)?, Some(&timer));
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
    let root = runtime.capture_observer_root(&runtime.inner.runtime_db)?;
    let selected = root
        .scheduling
        .as_ref()
        .unwrap()
        .capture(&SchedulingCaptureRequest::current())?;
    assert_eq!(selected.timer(&timer.id)?, Some(&timer));
    Ok(())
}

#[tokio::test]
async fn scoped_http_state_prepares_scheduling_after_active_metadata_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    publish_active_metadata_ack(&runtime).await?;
    let db = runtime
        .inner
        .runtime_db
        .with_read_scope(crate::runtime_db::RuntimeDbReadScope::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        ));
    let handle = tokio::runtime::Handle::current();
    let reader = runtime.clone();
    let id = tokio::task::spawn_blocking(move || -> Result<String> {
        // Exactly the loaded production /state preparation and capture path;
        // do not warm through agent_summary or an unscoped State read.
        let projection =
            reader.acquire_operator_state_read_projection_scoped(10, 10, &db, &handle)?;
        Ok(projection.agent.agent.id.clone())
    })
    .await??;
    assert_eq!(id, "scheduling-reader");
    Ok(())
}

#[tokio::test]
async fn scoped_child_summary_prepares_scheduling_after_active_metadata_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    publish_active_metadata_ack(&runtime).await?;
    let db = runtime
        .inner
        .runtime_db
        .with_read_scope(crate::runtime_db::RuntimeDbReadScope::new(
            std::time::Instant::now() + std::time::Duration::from_secs(30),
        ));
    let handle = tokio::runtime::Handle::current();
    let reader = runtime.clone();
    let id = tokio::task::spawn_blocking(move || -> Result<String> {
        Ok(reader
            .acquire_child_agent_summary_observation_scoped(&db, &handle)?
            .state
            .id
            .clone())
    })
    .await??;
    assert_eq!(id, "scheduling-reader");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observer_metadata_reconstruction_uses_readers_without_owner_or_writer_admission(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    publish_active_metadata_ack(&runtime).await?;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let _agent = runtime.inner.agent.lock().await;
    let (started, ready) = tokio::sync::oneshot::channel();
    let mut started = Some(started);
    let (release, resume) = std::sync::mpsc::channel();
    let db = runtime.inner.runtime_db.clone();
    let writer = tokio::task::spawn_blocking(move || {
        db.transaction(|_| {
            if let Some(started) = started.take() {
                let _ = started.send(());
            }
            resume.recv()?;
            Ok(())
        })
    });
    ready.await?;
    let observed = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.capture_observer_root_async(&runtime.inner.runtime_db),
    )
    .await??;
    assert!(Arc::ptr_eq(
        &before,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    assert!(before.current.is_none() && before.scheduling.is_none());
    let scheduling = observed
        .scheduling
        .as_ref()
        .unwrap()
        .capture(&SchedulingCaptureRequest::current())?;
    assert_eq!(scheduling.timer(&timer.id)?, Some(&timer));
    let reader = runtime.clone();
    let db = runtime.runtime_db().clone();
    let handle = tokio::runtime::Handle::current();
    let state = tokio::task::spawn_blocking(move || {
        reader.acquire_operator_state_read_projection_scoped(10, 10, &db, &handle)
    });
    let state = tokio::time::timeout(std::time::Duration::from_secs(5), state).await???;
    assert_eq!(state.agent.agent.id, "scheduling-reader");
    assert_eq!(state.timers.len(), 1);
    let child = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.child_agent_summary_observation(),
    )
    .await??;
    assert_eq!(child.state.id, "scheduling-reader");
    release.send(())?;
    writer.await??;
    drop(owner);
    Ok(())
}

#[tokio::test]
async fn observer_pending_publication_fails_without_waiting_or_reconciling() -> Result<()> {
    use crate::runtime::mutation_owner::{MutationOwner, MutationReservation};
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut owners = MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
    MutationReservation::begin_all(&mut owners, "pending-observer-operation".into())?;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let error = runtime
        .capture_observer_root(runtime.runtime_db())
        .unwrap_err();
    assert!(error
        .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
        .is_some());
    assert_eq!(denied.attempts(), 0);
    assert!(runtime.inner.mutation_owner.capture().is_err());
    MutationReservation::definitely_not_committed_all(&mut owners)?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn observer_publication_window_converges_for_bounded_async_reads() -> Result<()> {
    use crate::runtime::mutation_owner::{MutationOwner, MutationReservation};
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut owners = MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
    MutationReservation::begin_all(&mut owners, "bounded-observer-convergence".into())?;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let reader = runtime.clone();
    let db = runtime.inner.runtime_db.clone();
    let observed = tokio::task::spawn(async move { reader.observer_ready_root_bounded(&db).await });
    let started = tokio::time::Instant::now();
    // Let the bounded read observe the open window once and arm its retry.
    tokio::task::yield_now().await;
    MutationReservation::definitely_not_committed_all(&mut owners)?;
    let root = observed.await??;
    assert_eq!(root.state.id, "scheduling-reader");
    // Paused time only advances across the retry delay, so the read converged
    // through the window instead of never observing it.
    assert!(
        tokio::time::Instant::now().duration_since(started) >= std::time::Duration::from_millis(20)
    );
    assert_eq!(denied.attempts(), 0);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn bounded_observer_read_returns_original_busy_when_window_outlives_bound() -> Result<()> {
    use crate::runtime::mutation_owner::{MutationOwner, MutationReservation};
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut owners = MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
    MutationReservation::begin_all(&mut owners, "bounded-observer-exhausted".into())?;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let error = runtime
        .observer_ready_root_bounded(&runtime.inner.runtime_db)
        .await
        .unwrap_err();
    let busy = error
        .downcast_ref::<crate::runtime_db::RuntimeDbRetryableError>()
        .expect("window error keeps its retryable storage contract");
    assert_eq!(
        busy.operation(),
        RuntimeHandle::OBSERVER_PUBLICATION_OPERATION
    );
    assert_eq!(denied.attempts(), 0);
    MutationReservation::definitely_not_committed_all(&mut owners)?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn task_status_child_observation_crosses_transient_publication_window() -> Result<()> {
    use crate::runtime::mutation_owner::{MutationOwner, MutationReservation};
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut owners = MutationOwner::reserve_all(vec![runtime.inner.mutation_owner.clone()]).await?;
    MutationReservation::begin_all(&mut owners, "task-status-observer-window".into())?;
    let reader = runtime.clone();
    let observed =
        tokio::task::spawn(async move { reader.child_agent_summary_observation().await });
    // Give the observation its first failed attempt so the bounded wait is
    // armed before the transient window closes.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    MutationReservation::definitely_not_committed_all(&mut owners)?;
    let child = tokio::time::timeout(std::time::Duration::from_secs(5), observed).await???;
    assert_eq!(child.state.id, "scheduling-reader");
    Ok(())
}
