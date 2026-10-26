use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};
fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "scheduling-wire",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "scheduling-wire".into(),
        context_config(),
    )
}

#[tokio::test]
async fn actual_timer_tool_reads_hold_material_through_serialization_without_timer_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("actual timer wire".into()))
        .await?;
    drop(runtime.acquire_timer_record_read(&timer.id).await?);
    drop(runtime.acquire_scheduling_records_read(100).await?);
    // This output-only delta must not race the initial Context resource refresh.
    runtime.join_resource_worker().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let read = runtime.acquire_timer_record_read(&timer.id).await?;
    let owned = Arc::downgrade(&read._owned);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let wire = crate::tool::tools::serialize_success(
        crate::tool::names::GET_TIMER,
        read.record()?.unwrap(),
    )?;
    assert_eq!(wire.envelope.result.as_ref().unwrap()["id"], timer.id);
    assert!(owned.upgrade().is_some());
    drop(read);
    assert!(owned.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO wire_timers_forbidden; ALTER TABLE timer_wakes RENAME TO wire_wakes_forbidden; ALTER TABLE external_triggers RENAME TO wire_triggers_forbidden")?;Ok(())
    })?;
    let actual =
        crate::tool::tools::timer::get(&runtime, &serde_json::json!({"timer_id":timer.id})).await?;
    assert_eq!(actual.envelope.result.as_ref().unwrap()["id"], timer.id);
    let list = crate::tool::tools::timer::list(&runtime, &serde_json::json!({"limit":100})).await?;
    assert_eq!(
        list.envelope.result.as_ref().unwrap()["timers"][0]["id"],
        timer.id
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE wire_triggers_forbidden RENAME TO external_triggers; ALTER TABLE wire_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE wire_timers_forbidden RENAME TO timers")?;Ok(())
    })?;
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn exact_timer_query_rejects_foreign_before_body_decode_and_never_adopts_raw_late_timer(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let at = runtime.now();
    let foreign = TimerRecord {
        id: "foreign-wire-timer".into(),
        agent_id: "foreign-agent".into(),
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
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::repositories::upsert_timer_tx(tx, &foreign)?;
        tx.execute(
            "UPDATE timers SET payload_json='bad-foreign-body' WHERE timer_id=?1",
            [&foreign.id],
        )?;
        Ok(())
    })?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .acquire_timer_record_read(&foreign.id)
        .await
        .err()
        .context("foreign Timer cannot be exposed")?;
    assert_eq!(
        error
            .downcast_ref::<RuntimeError>()
            .unwrap()
            .descriptor()
            .code,
        "timer_not_found"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    let late = TimerRecord {
        id: "raw-late-own-timer".into(),
        agent_id: "scheduling-wire".into(),
        ..foreign
    };
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::repositories::upsert_timer_tx(tx, &late)?;
        Ok(())
    })?;
    let error = runtime
        .acquire_timer_record_read(&late.id)
        .await
        .err()
        .context("late raw Timer cannot be adopted")?;
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
            Some(
                crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::NeedsPreparation(
                    _
                )
            )
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let aliases:usize=runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_records WHERE record_id IN ('foreign-wire-timer','raw-late-own-timer')",
        [],|row|row.get(0))?;
    assert_eq!(aliases, 0);
    Ok(())
}

#[tokio::test]
async fn loaded_host_scheduling_dto_keeps_material_until_synchronous_wire_build() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.wait_for_bootstrap().await?;
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    drop(runtime.acquire_scheduling_records_read(50).await?);
    let projection = host
        .operator_agent_state_projection(runtime.instance_agent_id(), 40, 50)
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let material = projection
        .material
        ._scheduling
        .as_ref()
        .context("loaded Scheduling material missing")?;
    let owned = Arc::downgrade(&material._owned);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let value = serde_json::to_value(&projection.timers)?;
    assert_eq!(value[0]["id"], timer.id);
    assert!(owned.upgrade().is_some());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    drop(projection);
    assert!(owned.upgrade().is_none());
    runtime.cancel_timer(&timer.id).await?;
    drop(refresh);
    drop(run_loop);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn loaded_host_state_query_reconciles_original_timer_cancel_ack_before_its_read_token(
) -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("Known Timer cancellation".into()))
        .await?;
    drop(runtime.acquire_scheduling_records_read(50).await?);
    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
    );
    let error = runtime
        .cancel_timer(&timer.id)
        .await
        .err()
        .context("injected publication failure missing")?;
    assert!(
        format!("{error:#}").contains("publication requires reconciliation"),
        "{error:#}"
    );
    assert!(
        matches!(
            runtime.inner.mutation_owner.capture(),
            Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
        ),
        "original ACK must remain gated before query recovery"
    );
    let sql_snapshot = || -> Result<(String, u64, usize)> {
        let connection = runtime.runtime_db().connection()?;
        let status = connection.query_row(
            "SELECT status FROM timers WHERE timer_id=?1",
            [&timer.id],
            |row| row.get(0),
        )?;
        let revision = connection.query_row("SELECT revision FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            rusqlite::params![runtime.instance_agent_id(), runtime.inner.identity_incarnation], |row| row.get(0))?;
        let receipts =
            connection.query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |row| {
                row.get(0)
            })?;
        Ok((status, revision, receipts))
    };
    let before = sql_snapshot()?;
    assert_eq!(before.0, "cancelled");
    let projection = host
        .operator_agent_state_projection(runtime.instance_agent_id(), 40, 50)
        .await
        .map_err(|error| anyhow::anyhow!("{error:?}"))?;
    let cancelled = projection
        .timers
        .iter()
        .find(|record| record.id == timer.id)
        .context("recovered cancelled Timer missing")?;
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("query did not publish recovered root")?;
    let scheduling = root
        .scheduling
        .as_ref()
        .context("query dropped Scheduling")?;
    assert_eq!(scheduling.scope().agent_id, runtime.instance_agent_id());
    assert_eq!(
        scheduling.scope().identity_incarnation,
        root.token.identity_incarnation
    );
    assert_eq!(scheduling.coverage().revision, before.1);
    assert_eq!(root.state.as_ref(), &projection.agent.agent);
    assert_eq!(
        sql_snapshot()?,
        before,
        "state query must not rerun original Timer cancellation"
    );
    drop((root, projection));
    host.shutdown().await?;
    Ok(())
}
