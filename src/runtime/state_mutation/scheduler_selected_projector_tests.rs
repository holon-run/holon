use super::*;
use crate::runtime::current_read_facts::CurrentReadError;
use serde_json::Value;
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
fn queue_value(queue: &WorkQueueReadModel) -> Result<Value> {
    Ok(serde_json::to_value((
        &queue.current,
        &queue.queued_blocked,
        &queue.items,
        &queue.current_runnable,
        &queue.triggered_blocked,
        &queue.queued_runnable,
        &queue.yielded,
        &queue.waiting_for_operator,
        &queue.blocked,
        &queue.completing,
        &queue.completed_recent,
    ))?)
}

#[tokio::test]
async fn original_selected_scheduler_projection_is_shared_without_sql_or_root_retention_and_keeps_actual_output_peak(
) -> Result<()> {
    let home = tempfile::tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "selected-pure-projector",
        home.path().into(),
        crate::runtime::tests::support::context_config(),
    )?;
    runtime
        .create_work_item(
            "original selected projector Work".into(),
            None,
            None,
            Vec::new(),
        )
        .await?;
    {
        let mut owner = runtime.begin_mutation().await?;
        drop(
            owner
                .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
                .await?,
        );
    }
    runtime.refresh_context_resources().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let request = WorkCaptureRequest::current();
    let now = runtime.now();
    let read = runtime.capture_scheduler_read_from_snapshot_with_material(
        &root,
        &request,
        now,
        LIMITS,
        (0, 0),
        None,
    )?;
    let ledger = &runtime.inner.input_evidence_budget;
    let prior = read.material_estimate()?;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO selected_projector_forbidden_state;
         ALTER TABLE agent_identities RENAME TO selected_projector_forbidden_identity;
         ALTER TABLE work_items RENAME TO selected_projector_forbidden_work;
         ALTER TABLE tasks RENAME TO selected_projector_forbidden_tasks;
         ALTER TABLE wait_conditions RENAME TO selected_projector_forbidden_wait;
         ALTER TABLE messages RENAME TO selected_projector_forbidden_messages;
         ALTER TABLE briefs RENAME TO selected_projector_forbidden_briefs;
         ALTER TABLE queue_entries RENAME TO selected_projector_forbidden_queue;
         ALTER TABLE audit_events RENAME TO selected_projector_forbidden_events;
         ALTER TABLE timers RENAME TO selected_projector_forbidden_timers;
         ALTER TABLE external_triggers RENAME TO selected_projector_forbidden_triggers;",
    )?;
    let project = |limits| {
        let mut output =
            output_material::SchedulerOutputMaterial::new(ledger, &read.work.scope, limits, prior)?;
        let fields = project_selected_scheduler_with_output(
            ledger,
            &read.state,
            &read.work,
            &read.tasks,
            &read._sources._inputs,
            &read.scheduling,
            &read.current,
            &request,
            now,
            limits,
            &mut output,
        )?;
        let (own, guard) = output.finish();
        Ok::<_, anyhow::Error>((fields, own, guard))
    };
    let (fields, own, guard) = project(LIMITS)?;
    assert_eq!(fields.projection, read.projection);
    assert_eq!(
        queue_value(&fields.work_queue)?,
        queue_value(&read.work_queue)?
    );
    assert_eq!(fields.due_rechecks, read.due_rechecks);
    assert_eq!(fields.scheduling_posture, read.scheduling_posture);
    assert_eq!(fields.active_waits, read.active_waits);
    assert_eq!(
        serde_json::to_value(&fields.advisories)?,
        serde_json::to_value(&read.advisories)?
    );
    assert_eq!(fields.recent_brief_count, read.recent_brief_count);
    assert_eq!(
        fields.observability_briefs.len(),
        read.observability_briefs.len()
    );
    assert!(fields
        .observability_briefs
        .iter()
        .zip(&read.observability_briefs)
        .all(|(a, b)| Arc::ptr_eq(a, b)));
    let weak = Arc::downgrade(&guard);
    drop(fields);
    assert!(weak.upgrade().is_some());
    drop(guard);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let total = (
        usize::try_from(
            prior
                .0
                .checked_add(own.0)
                .context("measured projection bytes overflow")?,
        )?,
        prior
            .1
            .checked_add(own.1)
            .context("measured projection nodes overflow")?,
    );
    for limits in [(total.0 - 1, LIMITS.1), (LIMITS.0, total.1 - 1)] {
        let error = project(limits)
            .err()
            .context("selected projector exceeded its original joint phase")?;
        assert!(
            matches!(
                error.downcast_ref::<CurrentReadError>(),
                Some(CurrentReadError::ResourceExhausted)
            ) || matches!(
                error.downcast_ref::<crate::runtime::resident_scheduling::SchedulingReadError>(),
                Some(
                    crate::runtime::resident_scheduling::SchedulingReadError::ResourceExhausted(_)
                )
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        assert_eq!(Arc::strong_count(&root), strong);
        assert!(Arc::ptr_eq(
            &root,
            &owner.capture_ready_snapshot()?.unwrap()
        ));
    }
    drop(project((total.0, total.1))?);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(Arc::strong_count(&root), strong);
    Ok(())
}
