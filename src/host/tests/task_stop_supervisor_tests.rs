use super::*;
use crate::{
    host::task_stop_supervisor::StopSweepWorkers,
    runtime::resident_input_evidence::InputEvidenceScope,
    runtime_db::{task_snapshot::TaskDomainLoadRequest, transitions::PreparedInputTransition},
};

fn request() -> TaskDomainLoadRequest {
    TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    }
}

#[tokio::test]
async fn supervisor_original_retired_stop_without_task_queue_or_state_keeps_exact_ack_scope(
) -> Result<()> {
    let (_home, host) = test_host();
    let scope = InputEvidenceScope {
        agent_id: host.config().default_agent_id.clone(),
        identity_incarnation: 1,
    };
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
        Ok(())
    })?;
    let (source, source_guard) = host.runtime_db().original_stop_result_host_fixture(
        &scope,
        &host.inner.input_evidence_budget,
        false,
    )?;
    host.runtime_db().transaction(|tx| {
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&scope.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poisoned-successor-state' WHERE agent_id=?1",[&scope.agent_id])?;
        // Terminal Task is intentionally retained as original evidence. No
        // active Task or Queue predicate can supply this old Stop scope.
        let active:u64=tx.query_row("SELECT COUNT(*) FROM tasks WHERE status IN ('queued','running','cancelling')",[],|r|r.get(0))?;
        let queue:u64=tx.query_row("SELECT COUNT(*) FROM queue_entries",[],|r|r.get(0))?;
        assert_eq!((active,queue),(0,0));
        Ok(())
    })?;
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    assert!(owner.capture()?.is_none());
    let inventory = host.runtime_db().transaction(|tx| {
        crate::runtime_db::task_stop::pending_task_stop_host_scope_inventory_tx(
            tx,
            &request(),
            &host.inner.input_evidence_budget,
        )
    })?;
    assert_eq!(inventory.scopes().collect::<Vec<_>>(), vec![(&scope, 1)]);
    let (rows, guard, loaded) = host
        .load_task_stop_supervisor_scope(&scope, &request())
        .await?;
    assert!(loaded.is_none());
    assert_eq!(rows.records.len(), 1);
    assert_eq!(rows.records[0].effect_seq, source.effect_seq);
    assert_eq!(rows.records[0].intent.task.scope, scope);
    assert!(
        owner.capture()?.is_none(),
        "body discovery cannot publish Ready"
    );
    let state: String = host.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
        |r| r.get(0),
    )?;
    assert_eq!(state, "poisoned-successor-state");
    drop((inventory, rows, guard, source, source_guard));
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn supervisor_owned_workers_survive_abandoned_caller_and_shutdown_waits_for_registered_work(
) -> Result<()> {
    let (_home, host) = test_host();
    let registered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let completed = Arc::new(AtomicBool::new(false));
    let caller = tokio::spawn({
        let host = host.clone();
        let registered = registered.clone();
        let release = release.clone();
        let completed = completed.clone();
        async move {
            let worker_host = host.clone();
            host.run_owned_external_mutation(async move {
                let mut workers = StopSweepWorkers::new(&worker_host)?;
                workers
                    .admit(async move {
                        registered.notify_one();
                        release.notified().await;
                        completed.store(true, Ordering::Release);
                        Ok(())
                    })
                    .await;
                workers.finish().await
            })
            .await
        }
    });
    registered.notified().await;
    assert!(
        host.inner
            .input_evidence_budget
            .report()?
            .host_owned
            .metadata_estimate_bytes
            > 0
    );
    caller.abort();
    let _ = caller.await;
    let shutdown = tokio::spawn({
        let host = host.clone();
        async move { host.shutdown().await }
    });
    tokio::task::yield_now().await;
    assert!(!shutdown.is_finished());
    assert!(!completed.load(Ordering::Acquire));
    release.notify_one();
    shutdown.await??;
    assert!(completed.load(Ordering::Acquire));
    assert_eq!(
        host.inner
            .input_evidence_budget
            .report()?
            .host_owned
            .metadata_estimate_bytes,
        0
    );
    Ok(())
}

#[tokio::test]
async fn supervisor_worker_error_does_not_abort_other_owned_responsibilities() -> Result<()> {
    let (_home, host) = test_host();
    let completed = Arc::new(AtomicBool::new(false));
    let mut workers = StopSweepWorkers::new(&host)?;
    workers.admit(async { bail!("original-stop-retry") }).await;
    workers
        .admit({
            let completed = completed.clone();
            async move {
                completed.store(true, Ordering::Release);
                Ok(())
            }
        })
        .await;
    let error = workers.finish().await.err().context("worker failure")?;
    assert!(format!("{error:#}").contains("durable responsibility retained"));
    assert!(completed.load(Ordering::Acquire));
    assert_eq!(
        host.inner
            .input_evidence_budget
            .report()?
            .host_owned
            .metadata_estimate_bytes,
        0
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn supervisor_cold_no_state_result_is_unavailable_without_runtime_or_activation() -> Result<()>
{
    let (_home, host) = test_host();
    let scope = InputEvidenceScope {
        agent_id: host.config().default_agent_id.clone(),
        identity_incarnation: 1,
    };
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
        Ok(())
    })?;
    let (source, _source_guard) = host.runtime_db().original_stop_result_host_fixture(
        &scope,
        &host.inner.input_evidence_budget,
        false,
    )?;
    host.runtime_db().connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [&scope.agent_id],
    )?;
    host.sweep_pending_task_stops_at_startup().await?;
    let rows = host.runtime_db().transaction(|tx| {
        crate::runtime_db::task_stop::load_task_stop_by_sequence_tx(
            tx,
            &scope,
            source.effect_seq,
            request().byte_limit,
            &host.inner.input_evidence_budget,
        )
    })?;
    assert_eq!(
        rows.records[0].progress.result,
        crate::runtime_db::task_stop::TaskStopResultState::NeedsAttention
    );
    let phase = PreparedInputTransition::task_stop_result(
        &source,
        &request(),
        &host.inner.input_evidence_budget,
    )?;
    let receipt = host
        .runtime_db()
        .transaction(|tx| {
            crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(
                tx,
                phase.descriptor(),
            )
        })?
        .context("original phase receipt missing")?;
    let crate::runtime_db::input_mutation_receipts::InputMutationResult::TaskStopPhase { ack } =
        &receipt.outcome.result
    else {
        panic!("wrong phase outcome")
    };
    assert!(matches!(
        ack.result.as_ref().context("result ACK missing")?.outcome,
        crate::runtime_db::task_stop::TaskStopResultOutcome::Unavailable { .. }
    ));
    let counts:(u64,u64,u64)=host.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM agent_states),(SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
    assert_eq!(counts, (0, 0, 0));
    let registry = host.inner.runtimes.read().await;
    assert!(registry.agents.is_empty() && registry.temporary_runtimes.is_empty());
    drop(registry);
    let owner = host
        .bridge()
        .mutation_owner_for(&scope.agent_id, scope.identity_incarnation)?;
    assert!(owner.capture()?.is_none());
    let receipts: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |r| r.get(0),
    )?;
    host.sweep_pending_task_stops_at_startup().await?;
    let after: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        after, receipts,
        "finished original phase is not re-enqueued"
    );
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn supervisor_open_discovery_carries_loaded_original_runtime_without_replacing_monitor(
) -> Result<()> {
    let (_home, host) = test_host();
    let scope = InputEvidenceScope {
        agent_id: host.config().default_agent_id.clone(),
        identity_incarnation: 1,
    };
    host.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, &AgentState::new(&scope.agent_id))?;
        Ok(())
    })?;
    let (source, _guard) = host.runtime_db().original_stop_result_host_fixture(
        &scope,
        &host.inner.input_evidence_budget,
        false,
    )?;
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    let receipts: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |r| r.get(0),
    )?;
    let (selected, _selected_guard, loaded) = host
        .load_task_stop_supervisor_scope(&scope, &request())
        .await?;
    assert!(loaded
        .as_ref()
        .is_some_and(
            |loaded| loaded.instance_agent_id() == runtime.instance_agent_id()
                && loaded.instance_incarnation() == runtime.instance_incarnation()
        ));
    assert_eq!(selected.records[0].effect_seq, source.effect_seq);
    let after: u64 = host.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        after, receipts,
        "cold discovery cannot replace a live original worker"
    );
    let rows = host.runtime_db().transaction(|tx| {
        crate::runtime_db::task_stop::load_task_stop_by_sequence_tx(
            tx,
            &scope,
            source.effect_seq,
            request().byte_limit,
            &host.inner.input_evidence_budget,
        )
    })?;
    assert_eq!(
        rows.records[0].progress.result,
        crate::runtime_db::task_stop::TaskStopResultState::Pending
    );
    drop(rows);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn supervisor_retained_fleet_deduplicates_actual_instances_and_budgets_before_clones(
) -> Result<()> {
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    runtime.wait_for_bootstrap().await?;
    host.inner
        .runtimes
        .write()
        .await
        .unloading_mutations
        .insert(u64::MAX, runtime.clone());
    let (fleet, guard) = host
        .retained_task_stop_runtime_inventory(&request())
        .await?;
    assert_eq!(fleet.len(), 1);
    assert_eq!(
        fleet[0].original_process_registry_id(),
        runtime.original_process_registry_id()
    );
    assert!(
        host.inner
            .input_evidence_budget
            .report()?
            .host_owned
            .metadata_estimate_bytes
            > 0
    );
    drop((fleet, guard));
    let before = host.inner.input_evidence_budget.report()?.host_owned;
    let mut tiny = request();
    tiny.record_limit = 1;
    let error = host
        .retained_task_stop_runtime_inventory(&tiny)
        .await
        .err()
        .context("metadata budget")?;
    assert!(error
        .downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>()
        .is_some());
    assert_eq!(
        host.inner.input_evidence_budget.report()?.host_owned,
        before
    );
    host.inner
        .runtimes
        .write()
        .await
        .unloading_mutations
        .remove(&u64::MAX);
    host.shutdown().await?;
    Ok(())
}
