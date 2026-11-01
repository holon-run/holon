use super::*;
use crate::{
    runtime::{
        continue_original_task_stop_metadata, resident_input_evidence::InputEvidenceScope,
        resident_task::TaskCaptureRequest,
    },
    runtime_db::{
        task_snapshot::TaskDomainLoadRequest,
        task_stop::{TaskStopPhysicalState, TaskStopResultState},
    },
};
fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}
fn source(
    host: &RuntimeHost,
) -> Result<(
    Arc<crate::runtime_db::task_stop::TaskStopRecord>,
    Arc<crate::runtime::input_evidence_budget::BudgetViewGuard>,
)> {
    let scope = InputEvidenceScope {
        agent_id: host.config().default_agent_id.clone(),
        identity_incarnation: 1,
    };
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
        Ok(())
    })?;
    host.runtime_db()
        .original_stop_abort_host_fixture(&scope, &host.inner.input_evidence_budget)
}
#[tokio::test]
async fn cold_stop_result_failure_still_acknowledges_missing_monitor_and_retries_original_result_once(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, material) = source(&host)?;
    assert_eq!(original.progress.physical, TaskStopPhysicalState::Pending);
    host.runtime_db().connection()?.execute_batch("CREATE TRIGGER cold_stop_result_fault BEFORE INSERT ON input_mutation_receipts WHEN NEW.command_nonce='original-frozen-result-phase' BEGIN SELECT RAISE(ABORT,'cold-result-only-fault'); END;")?;
    let error = continue_original_task_stop_metadata(
        &host.bridge(),
        original.clone(),
        material.clone(),
        request(),
    )
    .await
    .err()
    .context("Result failure")?;
    assert!(format!("{error:#}").contains("durable responsibility retained"));
    let current = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert_eq!(
        current.original.progress.physical,
        TaskStopPhysicalState::NeedsAttention
    );
    assert_eq!(
        current.original.progress.result,
        TaskStopResultState::Pending
    );
    let physical_revision = current.original.revision;
    let counts: (u64, u64) = host.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(counts, (0, 0));
    host.runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER cold_stop_result_fault;")?;
    continue_original_task_stop_metadata(
        &host.bridge(),
        original.clone(),
        material.clone(),
        request(),
    )
    .await?;
    let current = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert_eq!(
        current.original.progress.physical,
        TaskStopPhysicalState::NeedsAttention
    );
    assert_eq!(
        current.original.progress.result,
        TaskStopResultState::Completed
    );
    assert_eq!(current.original.revision, physical_revision + 1);
    let phase_receipts: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |r| r.get(0),
    )?;
    continue_original_task_stop_metadata(&host.bridge(), original.clone(), material, request())
        .await?;
    let after: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        after, phase_receipts,
        "already finished facets cannot repeat"
    );
    let count: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE evidence_id='frozen-stop-feedback'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(count, 1);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn cold_stop_retired_original_is_unavailable_and_never_reads_poisoned_successor_state(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, material) = source(&host)?;
    host.runtime_db().transaction(|tx|{
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&original.intent.task.scope.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poisoned-successor-state' WHERE agent_id=?1",[&original.intent.task.scope.agent_id])?;Ok(())
    })?;
    continue_original_task_stop_metadata(&host.bridge(), original.clone(), material, request())
        .await?;
    let current = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert_eq!(
        current.original.progress.physical,
        TaskStopPhysicalState::NeedsAttention
    );
    assert_eq!(
        current.original.progress.result,
        TaskStopResultState::NeedsAttention
    );
    let (state, messages): (String, u64) = host.runtime_db().connection()?.query_row(
        "SELECT payload_json,(SELECT COUNT(*) FROM messages) FROM agent_states WHERE agent_id=?1",
        [&original.intent.task.scope.agent_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(state, "poisoned-successor-state");
    assert_eq!(messages, 0);
    let owner = host
        .bridge()
        .mutation_owner_for(&original.intent.task.scope.agent_id, 1)?;
    assert!(owner.capture()?.is_none());
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn original_stop_named_read_rejects_changed_binding_before_any_phase_and_releases_failed_guard(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, _material) = source(&host)?;
    let mut forged = original.as_ref().clone();
    forged.intent_digest = "0".repeat(64);
    let before = host.inner.input_evidence_budget.report()?.owned;
    let error = host
        .bridge()
        .read_original_task_stop_metadata(&forged, &request())
        .await
        .err()
        .context("immutable binding must fail")?;
    assert!(format!("{error:#}").contains("immutable address changed"));
    assert_eq!(host.inner.input_evidence_budget.report()?.owned, before);
    let current = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert_eq!(current.original.revision, original.revision);
    assert_eq!(current.original.progress, original.progress);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn original_stop_owner_availability_is_same_view_without_ready_or_successor_state_read(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, _source) = source(&host)?;
    let read = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert!(
        !read.owner_state_unavailable(),
        "State presence is not TaskReady"
    );
    drop(read);
    host.runtime_db().connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&original.intent.task.scope.agent_id],
    )?;
    let read = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert!(read.owner_state_unavailable());
    drop(read);
    host.runtime_db().transaction(|tx|{
        crate::runtime_db::evidence::upsert_agent_state_tx(tx,&AgentState::new(&original.intent.task.scope.agent_id))?;
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&original.intent.task.scope.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='unreadable-successor-state' WHERE agent_id=?1",[&original.intent.task.scope.agent_id])?;Ok(())
    })?;
    let read = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert!(read.owner_state_unavailable());
    let body: String = host.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&original.intent.task.scope.agent_id],
        |r| r.get(0),
    )?;
    assert_eq!(body, "unreadable-successor-state");
    let owner = host
        .bridge()
        .mutation_owner_for(&original.intent.task.scope.agent_id, 1)?;
    assert!(owner.capture()?.is_none());
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn original_stop_owner_identity_is_budgeted_before_its_json_or_state_scalar_observer(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, _source) = source(&host)?;
    host.runtime_db().connection()?.execute(
        "UPDATE agent_identities SET payload_json=?1 WHERE agent_id=?2",
        rusqlite::params!["{".repeat(8192), original.intent.task.scope.agent_id],
    )?;
    let before = host.inner.input_evidence_budget.report()?.owned;
    let mut tiny = request();
    tiny.byte_limit = 4096;
    let error = host
        .bridge()
        .read_original_task_stop_metadata(&original, &tiny)
        .await
        .err()
        .context("oversize bad identity")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
        Some(
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "original Stop owner identity"
            )
        )
    ));
    assert_eq!(host.inner.input_evidence_budget.report()?.owned, before);
    // Discovery failed before any phase ACK; no Ready or business effects.
    let owner = host
        .bridge()
        .mutation_owner_for(&original.intent.task.scope.agent_id, 1)?;
    assert!(owner.capture()?.is_none());
    let result_state: String = host.runtime_db().connection()?.query_row(
        "SELECT result_state FROM task_stop_effects WHERE effect_seq=?1",
        [original.effect_seq],
        |r| r.get(0),
    )?;
    assert_eq!(result_state, "pending");
    Ok(())
}

#[tokio::test]
async fn finished_stop_host_read_recovers_terminal_initial_ack_and_keeps_exact_old_epoch(
) -> Result<()> {
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    let finished = runtime
        .original_task_stop_finished_host_fixture("task:finished-old-owner")
        .await?;
    let scope = &finished.intent().task.scope;
    // A real ownership transfer fences the old Runtime. The Host reader must
    // reconcile the original witness through the new owner, without old begin.
    let (_new_owner, replacement) = host
        .bridge()
        .claim_reserved_mutation_owner(&scope.agent_id, scope.identity_incarnation)?;
    drop(replacement);
    assert!(runtime.agent_state().await.is_err());
    let current = host
        .bridge()
        .read_finished_original_task_stop_attempt(&finished, &request())
        .await?
        .context("committed initial Stop must remain observable")?;
    assert_eq!(current.record().prepared, *finished.prepared());
    assert_eq!(current.record().intent, *finished.intent());
    assert!(
        !current.record().progress.is_pending(),
        "NoHandle original ACK is terminal"
    );
    assert!(!current.owner_state_unavailable());
    runtime.continue_retained_task_stop_plans_owned().await?;
    let counts: (u64,u64) = host.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM task_stop_effects),(SELECT COUNT(*) FROM messages WHERE kind='task_result')", [],
        |r| Ok((r.get(0)?,r.get(1)?)),
    )?;
    assert_eq!(counts, (1, 0));
    drop(current);
    drop(finished);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn finished_stop_host_read_bounds_before_clone_and_preserves_original_after_failure(
) -> Result<()> {
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    let finished = runtime
        .original_task_stop_finished_host_fixture("task:finished-budget")
        .await?;
    let before = host.inner.input_evidence_budget.report()?.owned;
    let mut tiny = request();
    tiny.byte_limit = 1;
    let error = host
        .bridge()
        .read_finished_original_task_stop_attempt(&finished, &tiny)
        .await
        .err()
        .context("selector budget must reject before owner/read")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
        Some(
            crate::runtime_db::task_snapshot::TaskSnapshotError::ResourceExhausted(
                "finished original Stop selectors"
            )
        )
    ));
    assert_eq!(host.inner.input_evidence_budget.report()?.owned, before);
    let current = host
        .bridge()
        .read_finished_original_task_stop_attempt(&finished, &request())
        .await?
        .context("failed budget must not release original responsibility")?;
    assert_eq!(current.record().prepared, *finished.prepared());
    assert!(!current.record().progress.is_pending());
    drop(current);
    runtime.continue_retained_task_stop_plans_owned().await?;
    drop(finished);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn original_stop_current_callback_holds_exact_owner_without_promoting_a_cold_root(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, _source) = source(&host)?;
    let before = host.inner.input_evidence_budget.report()?.owned;
    let observed = host
        .bridge()
        .with_original_task_stop_metadata(&original, &request(), |reservation, current| {
            assert_eq!(reservation.agent_id(), original.intent.task.scope.agent_id);
            assert_eq!(
                reservation.identity_incarnation(),
                original.intent.task.scope.identity_incarnation
            );
            assert_eq!(current.record().prepared, original.prepared);
            assert_eq!(current.record().intent, original.intent);
            assert!(!current.owner_state_unavailable());
            assert!(reservation.capture_ready_snapshot()?.is_none());
            // A second owner cannot be claimed while this exact synchronous
            // publication observation is being consumed.
            assert!(host
                .bridge()
                .claim_reserved_mutation_owner(
                    reservation.agent_id(),
                    reservation.identity_incarnation(),
                )
                .is_err());
            Ok(current.record().effect_seq)
        })
        .await?;
    assert_eq!(observed, original.effect_seq);
    assert_eq!(host.inner.input_evidence_budget.report()?.owned, before);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    let error = host
        .bridge()
        .prepare_original_task_stop_publication(&original, &request())
        .await
        .err()
        .context("cold Stop must not construct a Runtime")?;
    assert!(format!("{error:#}").contains("no current Runtime"));
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn original_stop_named_preparation_uses_actual_current_runtime_and_keeps_complete_pending_ack(
) -> Result<()> {
    let (_home, host) = test_host();
    let (original, _source) = source(&host)?;
    let runtime = host.default_runtime().await?;
    host.bridge()
        .prepare_original_task_stop_publication(&original, &request())
        .await?;
    host.bridge()
        .with_original_task_stop_metadata(&original, &request(), |reservation, current| {
            assert!(runtime.owns_mutation_reservation(reservation));
            let root = reservation
                .capture_ready_snapshot()?
                .context("actual current Runtime root")?;
            assert_eq!(root.token.owner_epoch, reservation.owner_epoch());
            let tasks = root
                .tasks
                .as_ref()
                .context("named preparation must publish Tasks")?;
            let capture = tasks.capture_pending_stops()?;
            let actual = capture
                .pending_stops()?
                .find(|entry| entry.effect_seq == original.effect_seq)
                .context("complete pending Stop must include original ACK")?;
            assert_eq!(actual.as_ref(), current.record());
            Ok(())
        })
        .await?;
    // No local original lease exists in this fixture: publication alone must
    // neither signal nor turn the pending physical facet into a success.
    let current = host
        .bridge()
        .read_original_task_stop_metadata(&original, &request())
        .await?;
    assert_eq!(current.record().progress, original.progress);
    drop(current);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn actual_host_epoch_replacement_publishes_original_stop_and_joins_only_old_monitor_once(
) -> Result<()> {
    let (_home, host) = test_host();
    let old = host.default_runtime().await?;
    old.wait_for_bootstrap().await?;
    let original_admission = old.execution_admission_lease()?;
    let local = old
        .original_task_stop_lost_ack_epoch_host_fixture("task:stop-old-instance")
        .await?;
    let finished = local.finished_attempt()?;
    let initial = host
        .bridge()
        .read_finished_original_task_stop_attempt(&finished, &request())
        .await?
        .context("original initial ACK")?;
    assert_eq!(
        initial.record().progress.physical,
        TaskStopPhysicalState::Pending
    );
    assert_eq!(
        initial.record().progress.result,
        TaskStopResultState::Pending
    );
    assert!(!local.published()?);
    assert!(!local.signalled());
    let old_instance = old.original_process_registry_id();
    let old_epoch = old.instance_owner_epoch();
    let agent_id = old.instance_agent_id().to_owned();
    tokio::time::timeout(Duration::from_secs(5), host.unload_runtime(&agent_id)).await?;
    assert!(host.try_get_loaded_runtime(&agent_id).await.is_none());
    assert!(
        host.inner
            .runtimes
            .read()
            .await
            .unloading_mutations
            .values()
            .any(|runtime| runtime.original_process_registry_id() == old_instance),
        "actual in-flight admission must retain the original instance"
    );
    // Normal physical unload cancels/joins its original monitor before the Stop
    // is published. This must not manufacture a Stop signal/publication token.
    assert!(local.monitor_finished());
    assert!(!local.signalled());
    assert!(!local.published()?);
    let current = host.default_runtime().await?;
    current.wait_for_bootstrap().await?;
    assert_eq!(current.instance_incarnation(), old.instance_incarnation());
    assert_ne!(current.original_process_registry_id(), old_instance);
    assert!(current.instance_owner_epoch() > old_epoch);
    assert!(old.agent_state().await.is_err());
    let new_monitor = current
        .register_task_stop_actor_host_fixture("task:later-instance-producer")
        .await?;
    assert!(!new_monitor.is_finished());
    assert_ne!(
        Some(new_monitor.lease_id()),
        initial.record().intent.lease_id.as_deref()
    );
    host.bridge()
        .prepare_original_task_stop_publication(initial.record(), &request())
        .await?;
    assert!(
        !local.published()?,
        "preparation alone grants no local signal"
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        host.sweep_pending_task_stops_at_startup(),
    )
    .await??;
    let terminal = host
        .bridge()
        .read_original_task_stop_metadata(initial.record(), &request())
        .await?;
    assert_eq!(
        terminal.record().progress.physical,
        TaskStopPhysicalState::MonitorJoined
    );
    assert_eq!(
        terminal.record().progress.result,
        TaskStopResultState::Completed
    );
    assert!(!terminal.record().progress.is_pending());
    assert!(local.published()?);
    assert!(local.signalled());
    assert!(local.released_complete());
    assert!(!old.has_original_task_stop_plan(terminal.record())?);
    assert!(
        !new_monitor.is_finished(),
        "continuing the old Stop cannot abort a later actual producer"
    );
    let task_status: String = host.runtime_db().connection()?.query_row(
        "SELECT status FROM tasks WHERE task_id='task:later-instance-producer'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(task_status, "running");
    let result_nonce = &terminal
        .record()
        .intent
        .result
        .as_ref()
        .context("original Result")?
        .phase_nonce;
    let original_nonce = terminal.record().prepared.command_nonce();
    let counts: (u64,u64,u64) = host.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM task_stop_effects WHERE command_nonce=?1),(SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?2),(SELECT COUNT(*) FROM messages WHERE kind='task_result' AND task_id='task:stop-old-instance')",
        rusqlite::params![original_nonce,result_nonce],|row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
    )?;
    assert_eq!(counts, (1, 1, 1));
    let revision = terminal.record().revision;
    tokio::time::timeout(
        Duration::from_secs(10),
        host.sweep_pending_task_stops_at_startup(),
    )
    .await??;
    let again = host
        .bridge()
        .read_original_task_stop_metadata(terminal.record(), &request())
        .await?;
    assert_eq!(again.record().revision, revision);
    assert!(!new_monitor.is_finished());
    drop((again, terminal, initial, finished, local));
    drop(original_admission);
    host.shutdown().await?;
    assert!(new_monitor.is_finished());
    Ok(())
}

#[tokio::test]
async fn retired_resource_drain_preserves_unknown_current_owner_gate() -> Result<()> {
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let scope = (
        runtime.instance_agent_id().to_owned(),
        runtime.instance_incarnation(),
    );
    let (current, reserved) = host
        .bridge()
        .claim_reserved_mutation_owner(&scope.0, scope.1)?;
    let mut group = vec![reserved];
    crate::runtime::mutation_owner::MutationReservation::begin_all(
        &mut group,
        "retirement-unresolved-original".into(),
    )?;
    drop(group);
    runtime.close_execution_admission();
    let error = runtime
        .join_retiring_resources()
        .await
        .expect_err("current original Unknown must block durable reconciliation");
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime::mutation_owner::MutationOwnerError>(),
            Some(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ),
        "{error:#}"
    );
    assert!(matches!(
        current.capture(),
        Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
    ));
    assert!(host.shutdown().await.is_err());
    Ok(())
}

#[tokio::test]
async fn retired_resource_drain_keeps_original_join_failure_after_owner_replacement() -> Result<()>
{
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let (release, pending) = tokio::sync::oneshot::channel();
    runtime.spawn_owned_mutation(async move {
        let _ = pending.await;
        panic!("original retired worker panic");
    })?;
    let (current, reserved) = host.bridge().claim_reserved_mutation_owner(
        runtime.instance_agent_id(),
        runtime.instance_incarnation(),
    )?;
    drop(reserved);
    runtime.close_execution_admission();
    release.send(()).unwrap();
    let error = runtime
        .join_retiring_resources()
        .await
        .expect_err("old JoinError cannot become successful resource exit");
    assert!(
        error
            .downcast_ref::<tokio::task::JoinError>()
            .is_some_and(|join| join.is_panic()),
        "{error:#}"
    );
    let before = current.capture()?;
    runtime.join_retiring_resources().await?;
    let after = current.capture()?;
    assert!(
        match (&before, &after) {
            (Some(before), Some(after)) => Arc::ptr_eq(before, after),
            (None, None) => true,
            _ => false,
        },
        "resource drain must not manufacture or replace the current root"
    );
    host.shutdown().await?;
    Ok(())
}
