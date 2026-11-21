use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "timer-list-read",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "timer-list-read".into(),
        context_config(),
    )
}

async fn with_timer(home: &std::path::Path) -> Result<(RuntimeHandle, TimerRecord)> {
    let runtime = runtime(home)?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("list holder".into()))
        .await?;
    // Producers retain the runtime; isolate accounting without changing its State.
    runtime.join_idle_owned_tasks().await?;
    runtime.join_resource_worker().await?;
    Ok((runtime, timer))
}

#[tokio::test]
async fn public_timer_list_keeps_original_output_and_capture_until_last_holder_without_sql(
) -> Result<()> {
    let home = tempdir()?;
    let (runtime, timer) = with_timer(home.path()).await?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let original = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO list_timers_forbidden; ALTER TABLE timer_wakes RENAME TO list_wakes_forbidden; ALTER TABLE external_triggers RENAME TO list_triggers_forbidden")?;
        Ok(())
    })?;
    let read = Arc::new(runtime.recent_timers(1).await?);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0], timer);
    let output = Arc::downgrade(&read._material._owned);
    let selected = read._material._selected.as_ref().unwrap();
    assert_eq!(selected.records.len(), original.scheduling.records.len());
    for (selected, original) in selected.records.iter().zip(&original.scheduling.records) {
        assert!(Arc::ptr_eq(selected, original));
    }
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let wire = serde_json::to_value(&*read)?;
    assert_eq!(wire, serde_json::to_value(&read[..])?);
    assert!(wire.is_array());
    assert_eq!(wire[0]["id"], timer.id);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    let last = read.clone();
    drop(read);
    assert!(output.upgrade().is_some());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert_eq!(last[0], timer);
    drop(last);
    assert!(output.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE list_triggers_forbidden RENAME TO external_triggers; ALTER TABLE list_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE list_timers_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    Ok(())
}

#[tokio::test]
async fn empty_timer_lists_hold_output_and_history_rejection_releases_material() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    for limit in [0, 10] {
        let read = runtime.recent_timers(limit).await?;
        assert!(read.is_empty());
        assert_eq!(serde_json::to_string(&read)?, "[]");
        assert_eq!(format!("{read:?}"), "[]");
        let output = Arc::downgrade(&read._material._owned);
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes >= baseline + 512 * 4);
        drop(read);
        assert!(output.upgrade().is_none());
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    let error = runtime.recent_timers(usize::MAX).await.unwrap_err();
    assert!(matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::NeedsHistory)
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn failed_timer_list_serialization_does_not_release_live_material() -> Result<()> {
    struct RefuseWrite;
    impl std::io::Write for RefuseWrite {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("test serializer refused"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let home = tempdir()?;
    let (runtime, timer) = with_timer(home.path()).await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let read = runtime.recent_timers(1).await?;
    let output = Arc::downgrade(&read._material._owned);
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    assert!(serde_json::to_writer(RefuseWrite, &read)
        .unwrap_err()
        .is_io());
    assert!(output.upgrade().is_some());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert_eq!(read[0], timer);
    assert!(serde_json::to_value(&read)?.is_array());
    drop(read);
    assert!(output.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn timer_list_keeps_old_body_and_material_across_new_publication() -> Result<()> {
    let home = tempdir()?;
    let (runtime, timer) = with_timer(home.path()).await?;
    let old = runtime.recent_timers(1).await?;
    let output = Arc::downgrade(&old._material._owned);
    let selected = old._material._selected.as_ref().unwrap();
    let body = Arc::downgrade(&selected.records[0]);
    runtime.cancel_timer(&timer.id).await?;
    runtime.join_idle_owned_tasks().await?;
    let new = runtime.recent_timers(1).await?;
    assert_eq!(old[0].status, TimerStatus::Active);
    assert_eq!(new[0].status, TimerStatus::Cancelled);
    assert_eq!(old[0].id, new[0].id);
    assert!(!Arc::ptr_eq(
        &old._material._selected.as_ref().unwrap().records[0],
        &new._material._selected.as_ref().unwrap().records[0]
    ));
    drop(new);
    assert!(output.upgrade().is_some());
    assert!(body.upgrade().is_some());
    assert_eq!(old[0], timer);
    drop(old);
    assert!(output.upgrade().is_none());
    assert!(body.upgrade().is_none());
    Ok(())
}
