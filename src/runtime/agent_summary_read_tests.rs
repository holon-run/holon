use super::*;
use crate::runtime::tests::support::{
    context_config, isolate_test_user_home, tempdir, StubProvider,
};

fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    let runtime = RuntimeHandle::new(
        "summary-scheduling",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "summary-scheduling".into(),
        context_config(),
    )?;
    isolate_test_user_home(&runtime);
    Ok(runtime)
}

#[tokio::test]
async fn summary_prepares_invalidated_instructions_after_initial_resources() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    drop(runtime.agent_summary().await?);
    runtime
        .inner
        .resources
        .registry
        .invalidate(crate::runtime::resource_observation::ResourceInvalidation::KnownWrite)?;
    drop(runtime.agent_summary().await?);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let material = runtime.context_resource_material()?;
    runtime.capture_instruction_resources(
        &root.state,
        &runtime.inner.config_snapshot.load_full(),
        &material,
    )?;
    material.finish()?;
    runtime.join_owned_mutations().await
}

#[test]
fn summary_trigger_projection_borrows_source_and_latches_exact_output_capacity() -> Result<()> {
    use crate::{
        config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial,
        runtime::resident_input_evidence::InputEvidenceScope,
    };
    let active: ExternalTriggerRecord = serde_json::from_value(serde_json::json!({
        "external_trigger_id": "trigger 汉🦀 \"{}[],:\\ ",
        "target_agent_id": "summary-triggers",
        "delivery_mode": "wake_hint",
        "token": "not a summary field",
        "token_hash": "not a summary field either",
        "status": "active",
        "created_at": "2026-10-21T12:00:00Z",
        "delivery_count": 3
    }))?;
    let mut revoked = active.clone();
    revoked.status = ExternalTriggerStatus::Revoked;
    let scope = InputEvidenceScope {
        agent_id: "summary-triggers".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    fn shape(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len().saturating_sub(1)
                    + fields.values().map(|value| 2 + shape(value)).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let fields = serde_json::to_value((
        &active.external_trigger_id,
        &active.target_agent_id,
        &active.scope,
        &active.delivery_mode,
        &active.status,
        active.delivery_count,
        active.created_at,
        active.revoked_at,
        active.last_delivered_at,
    ))?;
    let slots = shape(&fields);
    let clone_bytes = serde_json::to_vec(&fields)?.len() as u64 * 4 + slots as u64 * 128 + 512;
    let bytes = ((scope.agent_id.len() + 256 + 4 * std::mem::size_of::<ExternalTriggerSummary>())
        * 8) as u64
        + clone_bytes * 8;
    let records = 64 + 4 * 8 + (slots + 16) * 8;
    for shortage in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            bytes - u64::from(shortage == Some(true)),
            records - usize::from(shortage == Some(false)),
        )?;
        let source = active.clone();
        let id = source.external_trigger_id.as_ptr();
        let target = source.target_agent_id.as_ptr();
        let sources = vec![revoked.clone(), source];
        let result = phase.read(|| active_trigger_summaries(&sources));
        if shortage.is_none() {
            let summaries = result?;
            assert_eq!(summaries.len(), 1);
            assert_eq!(sources[1].external_trigger_id.as_ptr(), id);
            assert_eq!(sources[1].target_agent_id.as_ptr(), target);
            assert_ne!(summaries[0].external_trigger_id.as_ptr(), id);
            assert_ne!(summaries[0].target_agent_id.as_ptr(), target);
            assert_eq!(
                summaries[0].external_trigger_id,
                sources[1].external_trigger_id
            );
            assert_eq!(summaries[0].target_agent_id, sources[1].target_agent_id);
            let wire = serde_json::to_value(&summaries[0])?;
            assert_eq!(wire["delivery_count"], 3);
            assert!(wire.get("token").is_none());
            assert!(wire.get("token_hash").is_none());
            assert_eq!(phase.finish()?, (bytes, records));
        } else {
            let error = result.unwrap_err();
            assert!(
                format!("{error:#}").contains("ResourceExhausted"),
                "{error:#}"
            );
            let held = ledger.report()?.observed_bytes;
            let retry = phase
                .read(|| active_trigger_summaries(Vec::new()))
                .unwrap_err();
            let latched = phase.finish().unwrap_err();
            assert_eq!(format!("{retry:#}"), format!("{latched:#}"));
            assert_eq!(ledger.report()?.observed_bytes, held);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
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
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
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
