use super::*;
use crate::runtime::mutation_owner::MutationOwnerError;
use crate::runtime::tests::support::{persist_task_transition, LifecycleHarness};
use crate::types::{
    AgentListObservationStatus, TaskKind, TaskRecord, TaskStatus, WorkItemPlanStatus,
};

async fn add_state_cut_records(
    runtime: &RuntimeHandle,
    suffix: &str,
) -> Result<(String, String, String)> {
    let work = runtime
        .create_work_item(
            format!("State observation cut {suffix}"),
            Some(WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await?;
    let task_id = format!("state-observation-task-{suffix}");
    persist_task_transition(
        runtime,
        &TaskRecord {
            id: task_id.clone(),
            agent_id: runtime.instance_agent_id().into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: runtime.now(),
            updated_at: runtime.now(),
            parent_message_id: None,
            work_item_id: Some(work.id.clone()),
            summary: Some(format!("State observation task {suffix}")),
            detail: None,
            recovery: None,
        },
        "state_observation_task_fixture",
    )
    .await?;
    let wait = runtime
        .register_wait_for(
            runtime.instance_agent_id(),
            Some(work.id),
            WaitForWakeKind::External,
            Some(format!("state-observation-external-{suffix}")),
            format!("State observation wait {suffix}"),
            None,
        )
        .await?;
    let timer = runtime
        .schedule_timer(
            3_600_000,
            None,
            Some(format!("State observation timer {suffix}")),
        )
        .await?;
    Ok((task_id, wait.condition.id, timer.id))
}

#[tokio::test]
async fn state_projection_keeps_task_wait_and_timer_records_on_one_published_cut() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let (task_a, wait_a, timer_a) = add_state_cut_records(runtime, "first").await?;
    let generation = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .token
        .generation;
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    let key = reservation.begin("state-material-open-commit-window".into())?;
    let first = runtime.operator_state_read_projection(10, 10).await?;
    let observation = first.observation.as_ref().unwrap();
    assert_eq!(observation.status, AgentListObservationStatus::Committing);
    assert_eq!(observation.publication_generation, generation);
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));
    reservation.definitely_not_committed()?;
    drop(reservation);
    let first_records = serde_json::to_value((&first.tasks, &first.waits, &first.timers))?;
    assert_eq!(first.tasks.len(), 1);
    assert_eq!(first.tasks[0].id, task_a);
    assert_eq!(first.waits.len(), 1);
    assert_eq!(first.waits[0].id, wait_a);
    assert_eq!(first.timers.len(), 1);
    assert_eq!(first.timers[0].id, timer_a);

    let (task_b, wait_b, timer_b) = add_state_cut_records(runtime, "second").await?;
    let second = runtime.operator_state_read_projection(10, 10).await?;
    assert!(
        second.observation.as_ref().unwrap().publication_generation > generation,
        "later domain publication must advance the State generation"
    );
    assert_eq!(second.tasks.len(), 2);
    assert!(second.tasks.iter().any(|task| task.id == task_a));
    assert!(second.tasks.iter().any(|task| task.id == task_b));
    assert_eq!(second.agent.active_task_count, 2);
    assert_eq!(second.waits.len(), 2);
    assert!(second.waits.iter().any(|wait| wait.id == wait_a));
    assert!(second.waits.iter().any(|wait| wait.id == wait_b));
    assert_eq!(second.timers.len(), 2);
    assert!(second.timers.iter().any(|timer| timer.id == timer_a));
    assert!(second.timers.iter().any(|timer| timer.id == timer_b));
    assert_eq!(second.active_timer_count, 2);
    assert_eq!(
        serde_json::to_value((&first.tasks, &first.waits, &first.timers))?,
        first_records,
        "later domain publication must not splice into an older State view"
    );
    Ok(())
}

#[tokio::test]
async fn state_wire_material_releases_after_last_wire_slice_and_detached_timer_loan() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    runtime
        .schedule_timer(
            3_600_000,
            None,
            Some("State observation last holder".into()),
        )
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    let generation = root.token.generation;
    drop(root);
    let mut reservation = runtime.inner.mutation_owner.reserve().await?;
    let key = reservation.begin("state-wire-release-open-commit-window".into())?;
    let projection = runtime.operator_state_read_projection(10, 10).await?;
    let observation = projection.observation.as_ref().unwrap();
    assert_eq!(observation.status, AgentListObservationStatus::Committing);
    assert_eq!(observation.publication_generation, generation);
    assert!(matches!(
        runtime.inner.mutation_owner.capture(),
        Err(MutationOwnerError::NeedsReconcile(actual)) if actual == key
    ));
    reservation.definitely_not_committed()?;
    drop(reservation);
    let guard = projection.material.acquisition.guard()?;
    let material = Arc::downgrade(&guard);
    drop(guard);
    let timer_loan = projection.timers[0].clone();
    let bytes = projection
        .material
        .into_json_bytes(&projection.agent.agent)?;
    let cloned = bytes.clone();
    let sliced = bytes.slice(1..bytes.len() - 1);
    drop(bytes);
    drop((
        projection.agent,
        projection.tasks,
        projection.waits,
        projection.timers,
        projection.external_triggers,
    ));
    runtime
        .append_original_audit_event(crate::types::AuditEvent::legacy(
            "runtime_error",
            serde_json::json!({}),
        ))
        .await?;
    assert!(old_root.upgrade().is_none());
    assert!(material.upgrade().is_some());
    drop(cloned);
    assert!(material.upgrade().is_some(), "the wire slice remains live");
    drop(sliced);
    assert!(
        material.upgrade().is_some(),
        "the detached Timer loan remains live"
    );
    drop(timer_loan);
    assert!(
        material.upgrade().is_none(),
        "the last holder releases material"
    );
    Ok(())
}
