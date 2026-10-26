//! Real creator + original monitor + actual typed Task ACK; no signal on mismatch.
use super::*;
use crate::runtime::control_cancellation::{ControlCancellationRegistry, SharedControlTaskHandle};
use crate::runtime::task_generation::{self, TaskRejoinGeneration};
use crate::runtime::tests::support::LifecycleHarness;
use crate::types::ChildAgentWorkspaceMode;
use tokio::sync::oneshot;

#[test]
fn original_task_rejoin_shape_is_shared_and_never_collapses_partial_or_invalid_markers(
) -> Result<()> {
    let at = "2026-10-14T00:00:00Z".parse()?;
    let mut task = TaskRecord {
        id: "original-partial".into(),
        agent_id: "default".into(),
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: Some(
            serde_json::json!({"rejoin_obligation_id":"original-partial","rejoin_generation":1}),
        ),
        recovery: None,
    };
    let registry = ControlCancellationRegistry::default();
    let original = registry.task(&task);
    assert!(original.matches_task_generation(&task));
    assert!(task.rejoin_fence().is_err());
    let mut next = task.clone();
    next.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    assert!(next.rejoin_fence().is_err());
    assert!(!original.matches_task_generation(&next));
    assert!(!task_generation::same_task_rejoin_generation(&task, &next));
    let record = registry.before_active_task_sql(&task, 1, 1);
    assert!(registry.registration_for_task(&task).is_some());
    assert!(registry.registration_for_task(&next).is_none());
    drop(record);
    let (cancel, cancelled) = oneshot::channel();
    let (force, forced) = oneshot::channel();
    let command = registry.command(
        &task,
        control_cancellation::SharedControlSignal::new(cancel),
        control_cancellation::SharedControlSignal::new(force),
    );
    assert!(command.matches_task_generation(&task));
    assert!(!command.matches_task_generation(&next));
    assert!(!command.cancellation_requested());
    drop((cancelled, forced));
    for (field, value) in [
        ("rejoin_obligation_id", serde_json::json!("foreign")),
        ("rejoin_obligation_id", serde_json::Value::Null),
        ("rejoin_generation", serde_json::json!(0)),
        ("rejoin_generation", serde_json::Value::Null),
        ("parent_turn_id", serde_json::Value::Null),
    ] {
        let mut invalid = task.clone();
        invalid.detail.as_mut().unwrap()[field] = value;
        assert!(TaskRejoinGeneration::of(&invalid).is_none());
        assert!(!task_generation::same_task_rejoin_generation(
            &invalid, &invalid
        ));
        assert!(!original.matches_task_generation(&invalid));
    }
    task.detail = Some(serde_json::json!({"parent_turn_id":"only-provenance"}));
    let mut markerless = task.clone();
    markerless.detail = None;
    assert!(task_generation::same_task_rejoin_generation(
        &task,
        &markerless
    ));
    Ok(())
}

#[test]
fn explicit_control_shapes_bind_original_partial_and_only_its_planned_terminal() -> Result<()> {
    let at = "2026-10-14T00:00:00Z".parse()?;
    let original = TaskRecord {
        id: "original-control-partial".into(),
        agent_id: "default".into(),
        kind: TaskKind::ActorInvocation,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: None,
        detail: Some(
            serde_json::json!({"rejoin_obligation_id":"original-control-partial","rejoin_generation":1}),
        ),
        recovery: None,
    };
    let terminal = tasks::prepare_lifecycle_stop_task_result(&original, at)?;
    assert!(original
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    assert_eq!(
        terminal.task.detail.as_ref().unwrap()["parent_turn_id"],
        serde_json::json!(terminal.message.turn_id.as_ref().unwrap())
    );
    let mut generation = serde_json::json!({"rejoin":null});
    generation[task_generation::CONTROL_ORIGINAL_REJOIN] =
        serde_json::to_value(TaskRejoinGeneration::of(&original).unwrap())?;
    generation[task_generation::CONTROL_TERMINAL_REJOIN] =
        serde_json::to_value(TaskRejoinGeneration::of(&terminal.task).unwrap())?;
    assert!(task_generation::control_original_rejoin_matches(
        &original,
        &generation
    ));
    assert!(task_generation::control_terminal_rejoin_matches(
        &terminal.task,
        &generation
    ));
    assert!(!task_generation::control_terminal_rejoin_matches(
        &original,
        &generation
    ));
    let mut next = terminal.task.clone();
    next.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    assert!(!task_generation::control_terminal_rejoin_matches(
        &next,
        &generation
    ));
    generation
        .as_object_mut()
        .unwrap()
        .remove(task_generation::CONTROL_TERMINAL_REJOIN);
    assert!(!task_generation::control_original_rejoin_matches(
        &original,
        &generation
    ));
    // Legacy null is readable as an original outcome, not a partial physical grant.
    assert!(!task_generation::control_original_rejoin_matches(
        &original,
        &serde_json::json!({"rejoin":null})
    ));
    let legacy = serde_json::json!({"rejoin":terminal.task.rejoin_fence().ok()});
    assert!(task_generation::control_terminal_rejoin_matches(
        &terminal.task,
        &legacy
    ));
    Ok(())
}

#[tokio::test]
async fn actual_partial_task_ack_cannot_make_public_stop_or_control_signal_the_old_monitor(
) -> Result<()> {
    for stop_task in [true, false] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        let original = runtime
            .create_agent_invocation_task(
                "original partial generation".into(),
                "accepted original".into(),
                AuthorityClass::OperatorInstruction,
                Some("original-target".into()),
                false,
                ChildAgentWorkspaceMode::Inherit,
            )
            .await?;
        assert_eq!(tasks::unbound_task_rejoin_generation(&original), Some(1));
        let (registration, lease, source) = runtime.admit_original_child_monitor(&original).await?;
        let (_send, done) = oneshot::channel::<()>();
        lease.attach(tokio::spawn(async move {
            let _ = done.await;
        }));
        runtime.inner.task_handles.lock().await.insert(
            original.id.clone(),
            command_task::ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
        );
        drop(registration);
        let mut next = original.clone();
        next.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
        next.updated_at = original.updated_at + chrono::Duration::milliseconds(1);
        write_child_task_for_test(runtime, &next).await?;
        let current = runtime.capture_current_task_record(&original.id)?;
        assert_eq!(
            tasks::unbound_task_rejoin_generation(current.record().context("Task missing")?),
            Some(2)
        );
        let before = runtime
            .runtime_db()
            .tasks()
            .latest(&original.id)?
            .context("actual Task missing")?;
        let receipt_count: usize = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |row| row.get(0),
        )?;
        let error = if stop_task {
            runtime
                .stop_task(&original.id, &AuthorityClass::OperatorInstruction)
                .await
                .unwrap_err()
        } else {
            runtime.control(ControlAction::Stop).await.unwrap_err()
        };
        assert!(format!("{error:#}").contains("generation"));
        assert!(!lease.cancellation_requested());
        assert!(!lease.is_finished());
        assert!(source.observed_terminal().is_none());
        assert_eq!(
            runtime.runtime_db().tasks().latest(&original.id)?,
            Some(before)
        );
        let (receipts, stops, controls):(usize,usize,usize)=runtime.runtime_db().connection()?.query_row(
            "SELECT (SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM task_stop_effects),(SELECT COUNT(*) FROM control_command_receipts)",
            [],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        assert_eq!((receipts, stops, controls), (receipt_count, 0, 0));
        lease.cancel();
        lease.join().await?;
        runtime
            .release_completed_task_handle_exact(&original.id, lease.lease_id())
            .await;
    }
    Ok(())
}
