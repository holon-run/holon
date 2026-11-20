use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

async fn with_timer(home: &std::path::Path) -> Result<(RuntimeHandle, TimerRecord)> {
    let runtime = RuntimeHandle::new(
        "timer-record-read",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "timer-record-read".into(),
        context_config(),
    )?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("record holder".into()))
        .await?;
    runtime.join_idle_owned_tasks().await?;
    runtime.join_resource_worker().await?;
    drop(runtime.latest_timer(&timer.id).await?);
    Ok((runtime, timer))
}

#[tokio::test]
async fn public_timer_record_keeps_complete_read_until_last_holder_without_sql() -> Result<()> {
    let home = tempdir()?;
    let (runtime, timer) = with_timer(home.path()).await?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let original = runtime
        .capture_scheduling_read(&SchedulingCaptureRequest::current())
        .await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE timers RENAME TO record_timers_forbidden; ALTER TABLE timer_wakes RENAME TO record_wakes_forbidden; ALTER TABLE external_triggers RENAME TO record_triggers_forbidden")?;
        Ok(())
    })?;
    let read = Arc::new(runtime.latest_timer(&timer.id).await?.unwrap());
    let borrowed: &TimerRecord = &read;
    assert_eq!(borrowed, &timer);
    let output = Arc::downgrade(&read._material._owned);
    let selector = Arc::downgrade(&read._material._selector);
    assert_eq!(
        read._material.selected.records.len(),
        original.scheduling.records.len()
    );
    for (selected, original) in read
        ._material
        .selected
        .records
        .iter()
        .zip(&original.scheduling.records)
    {
        assert!(Arc::ptr_eq(selected, original));
    }
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    assert_eq!(serde_json::to_value(&*read)?, serde_json::to_value(&timer)?);
    assert_eq!(format!("{:?}", &*read), format!("{timer:?}"));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    let last = read.clone();
    drop(read);
    assert!(output.upgrade().is_some());
    assert!(selector.upgrade().is_some());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert_eq!(last.status, TimerStatus::Active);
    drop(last);
    assert!(output.upgrade().is_none());
    assert!(selector.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE record_triggers_forbidden RENAME TO external_triggers; ALTER TABLE record_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE record_timers_forbidden RENAME TO timers")?;
        Ok(())
    })?;
    Ok(())
}

#[tokio::test]
async fn absent_public_timer_record_returns_null_and_releases_temporary_read() -> Result<()> {
    let home = tempdir()?;
    let (runtime, _) = with_timer(home.path()).await?;
    assert!(runtime.latest_timer("missing-record").await?.is_none());
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    for _ in 0..2 {
        let absent = runtime.latest_timer("missing-record").await?;
        assert!(absent.is_none());
        assert_eq!(serde_json::to_string(&absent)?, "null");
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().token,
            root.token
        );
        assert!(ResourceReadMaterial::active_guard()?.is_none());
    }
    Ok(())
}

#[tokio::test]
async fn public_timer_record_rejects_foreign_bad_body_and_never_adopts_raw_late_record(
) -> Result<()> {
    let home = tempdir()?;
    let (runtime, timer) = with_timer(home.path()).await?;
    let foreign = TimerRecord {
        id: "foreign-record".into(),
        agent_id: "foreign-owner".into(),
        ..timer.clone()
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
    let error = runtime.latest_timer(&foreign.id).await.unwrap_err();
    assert_eq!(
        error
            .downcast_ref::<RuntimeError>()
            .unwrap()
            .descriptor()
            .code,
        "timer_not_found"
    );
    let late = TimerRecord {
        id: "raw-late-record".into(),
        ..timer
    };
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::repositories::upsert_timer_tx(tx, &late)?;
        Ok(())
    })?;
    let error = runtime.latest_timer(&late.id).await.unwrap_err();
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
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    Ok(())
}

#[tokio::test]
async fn failed_timer_record_serialization_keeps_live_read_material() -> Result<()> {
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
    let read = runtime.latest_timer(&timer.id).await?.unwrap();
    let output = Arc::downgrade(&read._material._owned);
    let selector = Arc::downgrade(&read._material._selector);
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    assert!(serde_json::to_writer(RefuseWrite, &read)
        .unwrap_err()
        .is_io());
    assert!(output.upgrade().is_some());
    assert!(selector.upgrade().is_some());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert_eq!(serde_json::to_value(&read)?, serde_json::to_value(&timer)?);
    drop(read);
    assert!(output.upgrade().is_none());
    assert!(selector.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn public_timer_record_keeps_old_body_across_publication_and_runtime_drop() -> Result<()> {
    let home = tempdir()?;
    let (runtime, timer) = with_timer(home.path()).await?;
    let ledger = runtime.inner.input_evidence_budget.clone();
    let old = Arc::new(runtime.latest_timer(&timer.id).await?.unwrap());
    let output = Arc::downgrade(&old._material._owned);
    let selector = Arc::downgrade(&old._material._selector);
    let body = Arc::downgrade(&old._material.selected.records[0]);
    runtime.cancel_timer(&timer.id).await?;
    runtime.join_idle_owned_tasks().await?;
    let new = runtime.latest_timer(&timer.id).await?.unwrap();
    assert_eq!(old.status, TimerStatus::Active);
    assert_eq!(new.status, TimerStatus::Cancelled);
    assert_eq!(old.id, new.id);
    assert!(!Arc::ptr_eq(
        &old._material.selected.records[0],
        &new._material.selected.records[0]
    ));
    drop(new);
    drop(runtime);
    let held = ledger.report()?.observed_bytes;
    assert!(held > 0);
    let last = old.clone();
    drop(old);
    assert!(output.upgrade().is_some());
    assert!(selector.upgrade().is_some());
    assert!(body.upgrade().is_some());
    assert_eq!(ledger.report()?.observed_bytes, held);
    assert_eq!(serde_json::to_value(&*last)?, serde_json::to_value(&timer)?);
    drop(last);
    assert!(output.upgrade().is_none());
    assert!(selector.upgrade().is_none());
    assert!(body.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
