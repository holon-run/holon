use super::*;
use crate::runtime::{
    input_evidence_budget::serialized_bytes,
    resident_scheduling::{SchedulingCaptureRequest, SchedulingReadError},
    tests::support::{context_config, tempdir, StubProvider},
    timer_producer_reads::OwnedTimerProducerRead,
};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "timer-producer-read",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "timer-producer-read".into(),
        context_config(),
    )
}
async fn cancel_original_loops(runtime: &RuntimeHandle, id: &str) -> Result<()> {
    for lease in runtime.inner.control_cancellations.timer_for(id) {
        lease.cancel();
        lease.join().await?;
    }
    Ok(())
}

#[tokio::test]
async fn restart_timer_recovery_registers_from_qualified_capture_without_timer_body_sql(
) -> Result<()> {
    let home = tempdir()?;
    let first = runtime(home.path())?;
    let timer = first
        .schedule_timer(
            3_600_000,
            Some(3_600_000),
            Some("original restart Timer".into()),
        )
        .await?;
    cancel_original_loops(&first, &timer.id).await?;
    drop(first);
    let runtime = runtime(home.path())?;
    let selected = runtime
        .inner
        .recovered_timers
        .lock()
        .await
        .take()
        .context("bootstrap omitted selected Timer recovery")?;
    assert!(selected.coverage.active_timers_complete);
    assert_eq!(selected.timer(&timer.id)?.unwrap(), &timer);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(
        selected.scope.identity_incarnation,
        root.token.identity_incarnation
    );
    assert_eq!(
        selected.revision,
        root.scheduling.as_ref().unwrap().coverage().revision
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO producer_timers_forbidden; ALTER TABLE timer_wakes RENAME TO producer_wakes_forbidden; ALTER TABLE external_triggers RENAME TO producer_triggers_forbidden")?;
        Ok(())
    })?;
    runtime.recover_active_timers(selected).await?;
    assert_eq!(
        runtime
            .inner
            .control_cancellations
            .timer_for(&timer.id)
            .len(),
        1
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    cancel_original_loops(&runtime, &timer.id).await?;
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes < held);
    let original: (String, u64) = runtime.runtime_db().connection()?.query_row(
        "SELECT status,json_extract(payload_json,'$.fire_count') FROM producer_timers_forbidden WHERE timer_id=?1",
        [&timer.id], |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(original, ("active".into(), 0));
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE producer_triggers_forbidden RENAME TO external_triggers; ALTER TABLE producer_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE producer_timers_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn original_timer_copy_budget_is_combined_before_clone_and_guard_lives_with_producer(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("copy budget".repeat(128)))
        .await?;
    cancel_original_loops(&runtime, &timer.id).await?;
    let owner = runtime.begin_mutation().await?;
    let captured = owner.capture_scheduling_read(&SchedulingCaptureRequest::current())?;
    let selected = &captured.scheduling;
    let (retained, retained_records) = selected.selected_material_estimate()?;
    let selector = (serialized_bytes(&timer.id.as_str())? + 512) * 8;
    let copies = (serialized_bytes(&timer)? * 2 + 512) * 8;
    let full = retained + selector + copies;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let error = OwnedTimerProducerRead::from_selected(
        selected,
        &timer.id,
        Some(&timer),
        ledger,
        65536,
        usize::try_from(full - 1)?,
    )
    .err()
    .context("combined byte boundary accepted")?;
    assert!(matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::ResourceExhausted(_))
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let error = OwnedTimerProducerRead::from_selected(
        selected,
        &timer.id,
        Some(&timer),
        ledger,
        retained_records + 31,
        usize::try_from(full)?,
    )
    .err()
    .context("combined record boundary accepted")?;
    assert!(matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::ResourceExhausted(_))
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let read = OwnedTimerProducerRead::from_selected(
        selected,
        &timer.id,
        Some(&timer),
        ledger,
        65536,
        usize::try_from(full)?,
    )?;
    let actual = Arc::downgrade(&read.material);
    assert_eq!(read.timer, timer);
    assert!(actual.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(read);
    assert!(actual.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert!(runtime
        .inner
        .control_cancellations
        .timer_for(&timer.id)
        .is_empty());
    drop((captured, owner));
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}

#[tokio::test]
async fn registration_rejects_raw_late_timer_and_changed_original_generation_without_alias_or_signal(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    cancel_original_loops(&runtime, &timer.id).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let mut late = timer.clone();
    late.id = "raw-late-producer-timer".into();
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::repositories::upsert_timer_tx(tx, &late)?;
        tx.execute(
            "UPDATE timers SET payload_json='bad-late-body' WHERE timer_id=?1",
            [&late.id],
        )?;
        Ok(())
    })?;
    let error = runtime
        .spawn_timer_loop(late.clone())
        .await
        .err()
        .context("raw late Timer adopted")?;
    assert!(
        matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::NeedsPreparation(_))
        ),
        "{error:#}"
    );
    let mut wrong = timer.clone();
    wrong.created_at += chrono::Duration::milliseconds(1);
    let error = runtime
        .spawn_timer_loop(wrong)
        .await
        .err()
        .context("another original generation accepted")?;
    assert!(error.to_string().contains("another original generation"));
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(runtime
        .inner
        .control_cancellations
        .timer_for(&late.id)
        .is_empty());
    assert!(runtime
        .inner
        .control_cancellations
        .timer_for(&timer.id)
        .is_empty());
    let aliases: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_records WHERE record_id=?1",
        [&late.id],
        |row| row.get(0),
    )?;
    assert_eq!(aliases, 0);
    // This deliberately corrupt negative fixture must not enter a later full-domain writer.
    runtime.runtime_db().transaction(|tx| {
        tx.execute("DELETE FROM timers WHERE timer_id=?1", [&late.id])?;
        Ok(())
    })?;
    runtime.cancel_timer(&timer.id).await?;
    Ok(())
}
