use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "trigger-list-read",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "trigger-list-read".into(),
        context_config(),
    )
}

#[tokio::test]
async fn public_trigger_list_keeps_original_output_and_capture_until_last_holder_without_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let capability = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    runtime.join_resource_worker().await?;
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
    let read = Arc::new(runtime.latest_external_triggers().await?);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].external_trigger_id, capability.external_trigger_id);
    assert_eq!(read[0].status, ExternalTriggerStatus::Active);
    let output = Arc::downgrade(&read._material._owned);
    let selected = read._material._selected.as_ref().unwrap();
    assert!(Arc::ptr_eq(
        &selected.records[0],
        &original.scheduling.records[0]
    ));
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let wire = serde_json::to_value(&*read)?;
    assert_eq!(wire, serde_json::to_value(&read[..])?);
    assert_eq!(
        wire[0]["external_trigger_id"],
        capability.external_trigger_id
    );
    assert!(wire.is_array());
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
    assert_eq!(last[0].status, ExternalTriggerStatus::Active);
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
async fn empty_public_trigger_list_still_holds_its_output_and_preserves_array_json() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.join_resource_worker().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let read = runtime.latest_external_triggers().await?;
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
    Ok(())
}

#[tokio::test]
async fn failed_trigger_list_serialization_does_not_release_live_material() -> Result<()> {
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
    let runtime = runtime(home.path())?;
    let capability = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    runtime.join_resource_worker().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let read = runtime.latest_external_triggers().await?;
    let output = Arc::downgrade(&read._material._owned);
    let held = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let error = serde_json::to_writer(RefuseWrite, &read).unwrap_err();
    assert!(error.is_io());
    assert!(output.upgrade().is_some());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    assert_eq!(read[0].external_trigger_id, capability.external_trigger_id);
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
async fn trigger_list_keeps_old_body_and_material_across_new_publication() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let capability = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    runtime.join_resource_worker().await?;
    let old = runtime.latest_external_triggers().await?;
    let output = Arc::downgrade(&old._material._owned);
    let selected = old._material._selected.as_ref().unwrap();
    let body = Arc::downgrade(&selected.records[0]);
    runtime
        .revoke_external_trigger(&capability.external_trigger_id)
        .await?;
    let new = runtime.latest_external_triggers().await?;
    assert_eq!(old[0].status, ExternalTriggerStatus::Active);
    assert_eq!(new[0].status, ExternalTriggerStatus::Revoked);
    assert_eq!(old[0].external_trigger_id, new[0].external_trigger_id);
    assert!(!Arc::ptr_eq(
        &old._material._selected.as_ref().unwrap().records[0],
        &new._material._selected.as_ref().unwrap().records[0]
    ));
    drop(new);
    assert!(output.upgrade().is_some());
    assert!(body.upgrade().is_some());
    assert_eq!(old[0].status, ExternalTriggerStatus::Active);
    drop(old);
    assert!(output.upgrade().is_none());
    assert!(body.upgrade().is_none());
    Ok(())
}
