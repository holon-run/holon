use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "summary-scheduling",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "summary-scheduling".into(),
        context_config(),
    )
}

#[tokio::test]
async fn summary_trigger_projection_uses_original_scheduler_read_after_revoke_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let trigger = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let original = runtime.capture_scheduler_projection().await?;
    let original_revision = original.scheduling.revision;
    runtime
        .revoke_external_trigger(&trigger.external_trigger_id)
        .await?;
    let current = runtime.capture_scheduler_projection().await?;
    assert!(current.scheduling.revision > original_revision);
    let (old, old_material) = runtime.summary_triggers_from_capture(original.scheduling)?;
    let (new, new_material) = runtime.summary_triggers_from_capture(current.scheduling)?;
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].external_trigger_id, trigger.external_trigger_id);
    assert_eq!(old[0].status, ExternalTriggerStatus::Active);
    assert!(new.is_empty());
    assert_eq!(serde_json::to_value(&old)?[0]["status"], "active");
    drop((old, new, old_material, new_material));
    Ok(())
}

#[tokio::test]
async fn summary_and_get_agent_wire_hold_original_material_without_scheduling_body_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let trigger = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    drop(runtime.agent_summary().await?);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE external_triggers RENAME TO summary_triggers_forbidden; ALTER TABLE timers RENAME TO summary_timers_forbidden; ALTER TABLE timer_wakes RENAME TO summary_wakes_forbidden")?;
        Ok(())
    })?;
    let summary = runtime.agent_summary().await?;
    let owned = Arc::downgrade(&summary.material._owned[0]);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let original_dto = serde_json::to_value(&*summary)?;
    let wire = serde_json::to_value(&summary)?;
    assert_eq!(wire, original_dto);
    assert_eq!(
        wire["active_external_triggers"][0]["external_trigger_id"],
        trigger.external_trigger_id
    );
    assert!(owned.upgrade().is_some());
    drop((summary, wire, original_dto));
    assert!(owned.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    for detail in ["compact", "full"] {
        let wire = crate::tool::tools::get_agent::execute(
            &runtime,
            runtime.instance_agent_id(),
            &AuthorityClass::RuntimeInstruction,
            &serde_json::json!({"detail": detail}),
        )
        .await?;
        assert!(wire.envelope.result.as_ref().unwrap()["agent"].is_object());
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE summary_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE summary_timers_forbidden RENAME TO timers; ALTER TABLE summary_triggers_forbidden RENAME TO external_triggers")?;
        Ok(())
    })?;
    runtime
        .revoke_external_trigger(&trigger.external_trigger_id)
        .await?;
    Ok(())
}

#[tokio::test]
async fn summary_projection_rejects_combined_selected_and_output_before_body_clones() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    drop(runtime.agent_summary().await?);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let scheduling = {
        let read = runtime.capture_scheduler_projection().await?;
        read.scheduling
    };
    let (bytes, records) = scheduling.selected_material_estimate()?;
    let error = project_selected_scheduling_records(
        scheduling,
        0,
        &runtime.inner.input_evidence_budget,
        usize::try_from(bytes)?,
        records,
        (0, 0),
        Vec::new(),
    )
    .err()
    .context("selected alone fits but projected summary must not")?;
    assert!(matches!(
        error.downcast_ref::<super::super::resident_scheduling::SchedulingReadError>(),
        Some(super::super::resident_scheduling::SchedulingReadError::ResourceExhausted(_))
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
