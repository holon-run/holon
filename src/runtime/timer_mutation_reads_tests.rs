use super::*;
use crate::{
    runtime::{
        resident_scheduling::SchedulingCaptureRequest,
        tests::support::{context_config, tempdir, StubProvider},
        waiting::WaitForWakeKind,
        RuntimeHandle,
    },
    types::TimerStatus,
};
fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    let runtime = RuntimeHandle::new(
        "timer-routing",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "timer-routing".into(),
        context_config(),
    )?;
    // Timer routing must not depend on the host user's skill catalogue.
    let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
    config.user_home_dir = None;
    runtime.inner.config_snapshot.store(Arc::new(config));
    Ok(runtime)
}
async fn stop_original_loop(runtime: &RuntimeHandle, timer_id: &str) -> Result<()> {
    for lease in runtime.inner.control_cancellations.timer_for(timer_id) {
        lease.cancel();
        lease.join().await?;
    }
    Ok(())
}
fn sql_state(runtime: &RuntimeHandle, timer_id: &str) -> Result<(usize, usize, usize, u64, u64)> {
    let connection = runtime.runtime_db().connection()?;
    Ok((connection.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?,
        connection.query_row("SELECT COUNT(*) FROM queue_entries WHERE status='queued'", [], |r| r.get(0))?,
        connection.query_row("SELECT COUNT(*) FROM input_mutation_receipts", [], |r| r.get(0))?,
        connection.query_row("SELECT json_extract(payload_json,'$.fire_count') FROM timers WHERE timer_id=?1", [timer_id], |r| r.get(0))?,
        connection.query_row("SELECT revision FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",
            rusqlite::params![runtime.instance_agent_id(), runtime.inner.identity_incarnation], |r| r.get(0))?))
}
#[tokio::test]
async fn timer_route_uses_one_root_without_body_sql_and_preserves_fresh_merge_cancel_ack(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let _loop = runtime.inner.run_loop_test_gate.write().await;
    let work = runtime
        .create_work_item("same-root Timer wait".into(), None, None, vec![])
        .await?;
    runtime.pick_work_item(work.id.clone()).await?;
    let mut timer = runtime
        .schedule_timer(3_600_000, Some(3_600_000), Some("same-root wake".into()))
        .await?;
    stop_original_loop(&runtime, &timer.id).await?;
    let registration = runtime
        .register_wait_for(
            runtime.instance_agent_id(),
            Some(work.id.clone()),
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "original Timer wait".into(),
            None,
        )
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    let prepared = owner
        .prepare_timer_mutation_read(&timer.id, Some(&timer), true)
        .await?
        .context("Timer missing")?;
    assert_eq!(prepared.work_item_id.as_deref(), Some(work.id.as_str()));
    assert!(prepared.pending.is_none());
    drop(prepared);
    let token = owner.capture_published_root()?.token;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO routing_timers_forbidden; ALTER TABLE timer_wakes RENAME TO routing_wakes_forbidden; ALTER TABLE external_triggers RENAME TO routing_triggers_forbidden; ALTER TABLE work_items RENAME TO routing_work_forbidden; ALTER TABLE wait_conditions RENAME TO routing_wait_forbidden")?;
        Ok(())
    })?;
    let read = owner
        .capture_timer_mutation_read(&timer.id, Some(&timer), true)?
        .context("pure Timer missing")?;
    let guard = Arc::downgrade(&read.material);
    assert_eq!(read.timer, timer);
    assert_eq!(read.work_item_id.as_deref(), Some(work.id.as_str()));
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    assert_eq!(owner.capture_published_root()?.token, token);
    drop(read);
    assert!(guard.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE routing_wait_forbidden RENAME TO wait_conditions; ALTER TABLE routing_work_forbidden RENAME TO work_items; ALTER TABLE routing_triggers_forbidden RENAME TO external_triggers; ALTER TABLE routing_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE routing_timers_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    drop(owner);
    runtime.fire_timer_record(&mut timer).await?;
    let first = sql_state(&runtime, &timer.id)?;
    assert_eq!(first.3, 1);
    let owner = runtime.begin_mutation().await?;
    let route = owner
        .capture_timer_mutation_read(&timer.id, Some(&timer), true)?
        .context("actual fire root missing")?;
    let original_message = route
        .pending
        .as_ref()
        .context("actual pending Wake missing")?
        .message_id
        .clone();
    assert_eq!(route.pending.as_ref().unwrap().fire_count, 1);
    assert!(
        route.work_item_id.is_none(),
        "Triggered Wait is no longer an Active Timer route"
    );
    let work_read = owner.capture_work_read(&crate::runtime::work_reads::live_work_request())?;
    assert_eq!(
        work_read
            .work
            .wait_condition(&registration.condition.id)?
            .unwrap()
            .status,
        crate::types::WaitConditionStatus::Triggered
    );
    drop((route, work_read, owner));
    let receipt_counts = || -> Result<(usize, usize)> {
        Ok(runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(CASE WHEN family='timer' THEN 1 END),COUNT(CASE WHEN family='audit' THEN 1 END) FROM input_mutation_receipts",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?)
    };
    let before_merge_receipts = receipt_counts()?;
    runtime.fire_timer_record(&mut timer).await?;
    let merged = sql_state(&runtime, &timer.id)?;
    assert_eq!(
        (merged.0, merged.1),
        (first.0, first.1),
        "merge cannot add another Message or Queue source"
    );
    // The real Timer TX and its original post-commit Audit each own a receipt.
    assert_eq!(
        receipt_counts()?,
        (before_merge_receipts.0 + 1, before_merge_receipts.1 + 1)
    );
    assert_eq!(merged.2, first.2 + 2);
    assert_eq!(merged.3, 2);
    let owner = runtime.begin_mutation().await?;
    let route = owner
        .capture_timer_mutation_read(&timer.id, Some(&timer), false)?
        .unwrap();
    assert_eq!(route.pending.as_ref().unwrap().message_id, original_message);
    assert_eq!(route.pending.as_ref().unwrap().fire_count, 1);
    drop((route, owner));
    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
    );
    assert!(runtime.cancel_timer(&timer.id).await.is_err());
    assert_eq!(
        sql_state(&runtime, &timer.id)?,
        merged,
        "failed cancel must roll back the original whole transaction"
    );
    let cancelled = runtime.cancel_timer(&timer.id).await?;
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    let owner = runtime.begin_mutation().await?;
    let route = owner
        .capture_timer_mutation_read(&timer.id, Some(&timer), false)?
        .unwrap();
    assert!(route.pending.is_none());
    assert_eq!(route.timer.status, TimerStatus::Cancelled);
    assert_eq!(owner.state.pending, 0);
    Ok(())
}

#[tokio::test]
async fn timer_route_combines_selected_work_pending_and_owned_copies_before_clone() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let mut timer = runtime
        .schedule_timer(
            3_600_000,
            Some(3_600_000),
            Some("owned material".repeat(128)),
        )
        .await?;
    stop_original_loop(&runtime, &timer.id).await?;
    runtime.fire_timer_record(&mut timer).await?;
    let owner = runtime.begin_mutation().await?;
    let selected = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    let work = owner.capture_work_read(&crate::runtime::work_reads::live_work_request())?;
    let (scheduling_bytes, scheduling_records) =
        selected.scheduling.selected_material_estimate()?;
    let (work_bytes, work_records) = selected_work_material(&work.work)?;
    let pending = selected.scheduling.pending_wake(&timer.id)?.unwrap();
    let selector = (serialized_bytes(&timer.id.as_str())? + 512) * 8;
    let copies = (serialized_bytes(&timer)? * 2 + 512 + serialized_bytes(pending)?) * 32;
    let bytes = scheduling_bytes + work_bytes + selector + copies;
    let records = scheduling_records + work_records + 136;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    for (record_limit, byte_limit) in [
        (records, usize::try_from(bytes - 1)?),
        (records - 1, usize::try_from(bytes)?),
    ] {
        let error = TimerMutationRead::from_selected(
            &selected.scheduling,
            Some(&work.work),
            &timer.id,
            Some(&timer),
            ledger,
            record_limit,
            byte_limit,
        )
        .err()
        .context("combined Timer routing boundary accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<SchedulingReadError>(),
                Some(SchedulingReadError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let read = TimerMutationRead::from_selected(
        &selected.scheduling,
        Some(&work.work),
        &timer.id,
        Some(&timer),
        ledger,
        records,
        usize::try_from(bytes)?,
    )?
    .unwrap();
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((selected, work, owner));
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn accepted_timer_route_rejects_raw_late_and_old_generation_without_alias_or_signal(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    stop_original_loop(&runtime, &timer.id).await?;
    runtime.join_resource_worker().await?;
    let mut owner = runtime.begin_mutation().await?;
    drop(
        owner
            .prepare_timer_mutation_read(&timer.id, Some(&timer), true)
            .await?,
    );
    let root = owner.capture_published_root()?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let mut old = timer.clone();
    old.created_at -= chrono::Duration::milliseconds(1);
    let error = owner
        .capture_timer_mutation_read(&timer.id, Some(&old), false)
        .err()
        .context("old generation accepted")?;
    assert!(format!("{error:#}").contains("another original generation"));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let mut raw = timer.clone();
    raw.id = "raw-late-routing-timer".into();
    runtime.runtime_db().timers().upsert(&raw)?;
    let error = owner
        .prepare_timer_mutation_read(&raw.id, None, false)
        .await
        .err()
        .context("raw late Timer adopted")?;
    assert!(
        matches!(error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
        Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::NeedsPreparation(key)) if key.id == raw.id),
        "{error:#}"
    );
    assert_eq!(owner.capture_published_root()?.token, root.token);
    let aliases: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_records WHERE record_id=?1",
        [&raw.id],
        |r| r.get(0),
    )?;
    assert_eq!(aliases, 0);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(runtime
        .inner
        .control_cancellations
        .timer_for(&raw.id)
        .is_empty());
    runtime
        .runtime_db()
        .connection()?
        .execute("DELETE FROM timers WHERE timer_id=?1", [&raw.id])?;
    drop((root, owner));
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn timer_cancel_no_state_keeps_original_pending_responsibility_and_ready_root_unchanged(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let mut timer = runtime
        .schedule_timer(3_600_000, Some(3_600_000), None)
        .await?;
    stop_original_loop(&runtime, &timer.id).await?;
    runtime.fire_timer_record(&mut timer).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let before = sql_state(&runtime, &timer.id)?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    // Deliberate fixture corruption; ordinary Timer mutation cannot synthesize State.
    runtime.runtime_db().connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [runtime.instance_agent_id()],
    )?;
    let error = runtime
        .cancel_timer(&timer.id)
        .await
        .err()
        .context("NoState Timer cancellation accepted")?;
    assert!(
        matches!(
            error.downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>(),
            Some(crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError::RetiredScope)
        ),
        "{error:#}"
    );
    assert_eq!(sql_state(&runtime, &timer.id)?, before);
    let current = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(current.token, root.token);
    assert!(Arc::ptr_eq(
        current.scheduling.as_ref().unwrap(),
        root.scheduling.as_ref().unwrap()
    ));
    let states: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM agent_states WHERE agent_id=?1",
        [runtime.instance_agent_id()],
        |r| r.get(0),
    )?;
    assert_eq!(states, 0);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_state_tx(tx, root.state.as_ref())
    })?;
    drop((root, current));
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

async fn claim_fixture() -> Result<(tempfile::TempDir, RuntimeHandle, TimerRecord)> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let mut timer = runtime
        .schedule_timer(3_600_000, Some(3_600_000), Some("original claim".into()))
        .await?;
    stop_original_loop(&runtime, &timer.id).await?;
    runtime.fire_timer_record(&mut timer).await?;
    Ok((home, runtime, timer))
}

#[tokio::test]
async fn timer_queue_claim_selects_published_pending_tuple_without_body_sql() -> Result<()> {
    let (_home, runtime, timer) = claim_fixture().await?;
    let request = SchedulingCaptureRequest {
        include_live: true,
        history_limit: 0,
        required_keys: Default::default(),
    };
    let mut owner = runtime.begin_mutation().await?;
    owner.prepare_owner_scheduling_request(&request).await?;
    let work_request = crate::runtime::work_reads::live_work_request();
    owner.prepare_owner_work_request(&work_request).await?;
    let read = owner.capture_scheduling_read(&request)?;
    let (work_token, _, work) = owner.capture_work_with_state(&work_request)?;
    assert_eq!(read.token, work_token);
    let pending = read
        .scheduling
        .pending_wake(&timer.id)?
        .context("original Wake missing")?;
    let source = owner
        .queue
        .shared_messages()
        .find(|message| message.id == pending.message_id)
        .cloned()
        .context("actual accepted wake source missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO claim_timers_forbidden; ALTER TABLE timer_wakes RENAME TO claim_wakes_forbidden; ALTER TABLE external_triggers RENAME TO claim_triggers_forbidden; ALTER TABLE work_items RENAME TO claim_work_forbidden; ALTER TABLE wait_conditions RENAME TO claim_waits_forbidden")?;
        Ok(())
    })?;
    let claim = TimerWakeClaimRead::from_selected(
        &read.scheduling,
        &work,
        &source,
        ledger,
        65_536,
        64 * 1024 * 1024,
    )?
    .context("exact pending Wake not selected")?;
    assert_eq!(claim.claim.timer_id, timer.id);
    assert_eq!(claim.claim.message_id, source.id);
    assert_eq!(claim.claim.fire_count, pending.fire_count);
    let guard = Arc::downgrade(&claim.material);
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(claim);
    assert!(guard.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(owner.capture_published_root()?.token, read.token);
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE claim_waits_forbidden RENAME TO wait_conditions; ALTER TABLE claim_work_forbidden RENAME TO work_items; ALTER TABLE claim_triggers_forbidden RENAME TO external_triggers; ALTER TABLE claim_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE claim_timers_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    drop((read, work, owner));
    let crate::runtime::scheduler_executor::RunLoopPoll::Message(scheduled) =
        crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await?
    else {
        anyhow::bail!("actual Timer source was not claimed")
    };
    assert_eq!(scheduled.message.id, source.id);
    assert!(runtime
        .runtime_db()
        .timers()
        .pending_wake(&timer.id)?
        .is_none());
    let state: String = runtime.runtime_db().connection()?.query_row(
        "SELECT status FROM timer_wakes WHERE timer_id=?1 AND message_id=?2",
        rusqlite::params![timer.id, source.id],
        |row| row.get(0),
    )?;
    assert_eq!(state, "incorporated");
    let owner = runtime.begin_mutation().await?;
    let captured = owner.capture_scheduling_read(&request)?;
    let work = owner.capture_work(&work_request)?;
    assert!(TimerWakeClaimRead::from_selected(
        &captured.scheduling,
        &work,
        &source,
        ledger,
        65_536,
        64 * 1024 * 1024
    )?
    .is_none());
    drop((captured, work, owner));
    let processing = runtime.clone();
    tokio::spawn(async move {
        let crate::runtime::scheduler_executor::ScheduledMessage {
            message,
            running_state,
            dispatch_plan,
            scheduler_decision,
            run_completion,
            _reply_owner_budget,
        } = scheduled;
        let run = running_state
            .current_run_id
            .as_deref()
            .context("original claim Run missing")?;
        let started = std::time::Instant::now();
        let terminal = processing
            .process_message_with_plan_deferred(message.clone(), dispatch_plan, &scheduler_decision)
            .await;
        processing
            .finish_scheduled_message_processing(&message, run, started, terminal)
            .await?;
        drop(run_completion);
        Ok::<(), anyhow::Error>(())
    })
    .await??;
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&source.id)?
            .unwrap()
            .status,
        crate::types::QueueEntryStatus::Processed
    );
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn timer_claim_combines_selected_domains_and_owned_tuple_before_allocation() -> Result<()> {
    let (_home, runtime, timer) = claim_fixture().await?;
    let owner = runtime.begin_mutation().await?;
    let mut scheduling = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    let work = owner.capture_work(&crate::runtime::work_reads::live_work_request())?;
    let wake = scheduling
        .scheduling
        .pending_wake(&timer.id)?
        .context("actual Wake missing")?;
    let source = owner
        .queue
        .shared_messages()
        .find(|message| message.id == wake.message_id)
        .cloned()
        .unwrap();
    let (a, x) = scheduling.scheduling.selected_material_estimate()?;
    let (b, y) = selected_work_material(&work)?;
    let bytes = a + b + (serialized_bytes(&(&timer.id, &source.id, wake.fire_count))? + 512) * 32;
    let records = x + y + 32;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    for (records, bytes) in [(records - 1, bytes), (records, bytes - 1)] {
        let error = TimerWakeClaimRead::from_selected(
            &scheduling.scheduling,
            &work,
            &source,
            ledger,
            records,
            usize::try_from(bytes)?,
        )
        .err()
        .context("combined claim allowance accepted")?;
        assert!(
            matches!(
                error.downcast_ref::<SchedulingReadError>(),
                Some(SchedulingReadError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let accepted = TimerWakeClaimRead::from_selected(
        &scheduling.scheduling,
        &work,
        &source,
        ledger,
        records,
        usize::try_from(bytes)?,
    )?
    .unwrap();
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(accepted);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut stale = source.as_ref().clone();
    stale.id.push_str("-not-original-Wake");
    assert!(TimerWakeClaimRead::from_selected(
        &scheduling.scheduling,
        &work,
        &stale,
        ledger,
        65_536,
        64 * 1024 * 1024
    )?
    .is_none());
    stale.agent_id = "another-agent".into();
    assert!(TimerWakeClaimRead::from_selected(
        &scheduling.scheduling,
        &work,
        &stale,
        ledger,
        65_536,
        64 * 1024 * 1024
    )
    .is_err());
    // Pure negative fixture: incomplete metadata cannot trigger a SQL lookup.
    scheduling.scheduling.coverage.pending_wakes_complete = false;
    let error = TimerWakeClaimRead::from_selected(
        &scheduling.scheduling,
        &work,
        &source,
        ledger,
        65_536,
        64 * 1024 * 1024,
    )
    .err()
    .context("incomplete pending coverage accepted")?;
    assert!(matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::IncompleteLive)
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    drop((scheduling, work, owner));
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn actual_timer_claim_fault_rolls_back_incorporation_and_all_original_queue_writes(
) -> Result<()> {
    let (_home, runtime, timer) = claim_fixture().await?;
    let original = runtime
        .runtime_db()
        .timers()
        .pending_wake(&timer.id)?
        .context("actual Wake missing")?;
    let state = runtime
        .runtime_db()
        .agent_states()
        .latest(runtime.instance_agent_id())?
        .unwrap();
    let counts = sql_state(&runtime, &timer.id)?;
    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::AfterCanonicalWrites,
    );
    assert!(
        crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
            .poll()
            .await
            .is_err()
    );
    assert_eq!(
        runtime
            .runtime_db()
            .timers()
            .pending_wake(&timer.id)?
            .unwrap(),
        original
    );
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&original.message_id)?
            .unwrap()
            .status,
        crate::types::QueueEntryStatus::Queued
    );
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest(runtime.instance_agent_id())?
            .unwrap(),
        state
    );
    assert_eq!(sql_state(&runtime, &timer.id)?, counts);
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}
