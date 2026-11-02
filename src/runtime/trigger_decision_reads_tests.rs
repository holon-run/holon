use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};

#[tokio::test]
async fn trigger_decision_selected_and_owned_material_share_one_remaining_allowance() -> Result<()>
{
    let home = tempdir()?;
    let runtime = RuntimeHandle::new(
        "default",
        home.path().into(),
        None::<crate::types::WorkspaceEntry>,
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("unused")),
        "default".into(),
        context_config(),
    )?;
    let _work = runtime
        .create_work_item("selected Work".into(), None, None, vec![])
        .await?;
    let capability = runtime
        .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
        .await?;
    let owner = runtime.begin_mutation().await?;
    let captured = owner.capture_scheduling_read(&SchedulingCaptureRequest {
        include_live: true,
        history_limit: 0,
        required_keys: Default::default(),
    })?;
    let work = owner.capture_work(&crate::runtime::work_reads::live_work_request())?;
    let trigger = captured
        .scheduling
        .active_default_trigger()?
        .context("actual Trigger absent")?;
    let (selected, count) = captured.scheduling.selected_material_estimate()?;
    let (work_bytes, work_records) = selected_work_material(&work)?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let active_bytes = selected + work_bytes + (serialized_bytes(&Some(trigger))? + 512) * 32;
    let active_records = count + work_records + 32;
    for (records, bytes) in [
        (active_records - 1, active_bytes),
        (active_records, active_bytes - 1),
    ] {
        let error = ActiveTriggerDecisionRead::from_selected(
            &captured.scheduling,
            &work,
            ledger,
            records,
            usize::try_from(bytes)?,
        )
        .err()
        .context("combined Focus Trigger material accepted")?;
        assert!(matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::ResourceExhausted(_))
        ));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let base_url = runtime.inner.callback_base_url.as_str();
    let url_bytes = selected
        + (u64::try_from(base_url.len() + trigger.token.as_ref().unwrap().len())? + 512) * 32;
    let url_records = count + 32;
    for (records, bytes) in [(url_records - 1, url_bytes), (url_records, url_bytes - 1)] {
        let error = CommandTriggerRead::from_selected(
            &captured.scheduling,
            base_url,
            ledger,
            records,
            usize::try_from(bytes)?,
        )
        .err()
        .context("combined URL material accepted")?;
        assert!(matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::ResourceExhausted(_))
        ));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let accepted = CommandTriggerRead::from_selected(
        &captured.scheduling,
        base_url,
        ledger,
        url_records,
        usize::try_from(url_bytes)?,
    )?;
    assert_eq!(
        accepted.url.as_deref(),
        Some(capability.trigger_url.as_str())
    );
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(accepted);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
