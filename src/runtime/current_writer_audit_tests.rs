use super::*;
use crate::storage::{EventBus, PublishedAuditEvent};
use tokio::sync::broadcast;

fn subscribe(runtime: &RuntimeHandle) -> Result<broadcast::Receiver<PublishedAuditEvent>> {
    let bus = EventBus::new(128);
    runtime.storage().enable_event_bus(bus.clone())?;
    Ok(bus.subscribe())
}

fn drain(
    runtime: &RuntimeHandle,
    rx: &mut broadcast::Receiver<PublishedAuditEvent>,
) -> Result<Vec<String>> {
    let scope = resident_input_evidence::InputEvidenceScope {
        agent_id: "default".into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let mut kinds = Vec::new();
    let mut previous = 0;
    loop {
        match rx.try_recv() {
            Ok(published) => {
                assert_eq!(published.agent_id.as_deref(), Some("default"));
                let qualified = published.qualified.as_ref().context("raw business Audit")?;
                qualified.validate(&scope, &runtime.inner.input_evidence_budget)?;
                assert!(std::ptr::eq(qualified.event(), &*published.event));
                assert!(published.event.event_seq > previous);
                previous = published.event.event_seq;
                kinds.push(published.event.kind.clone());
            }
            Err(broadcast::error::TryRecvError::Empty) => return Ok(kinds),
            Err(error) => return Err(error.into()),
        }
    }
}

#[tokio::test]
async fn independent_audit_writer_callback_full_chain_publishes_qualified_originals() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut rx = subscribe(runtime)?;
    let capability = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    let delivered = runtime
        .deliver_callback(
            &capability.external_trigger_id,
            CallbackDeliveryPayload {
                body: None,
                content_type: None,
                correlation_id: Some("original-callback-correlation".into()),
                causation_id: None,
            },
        )
        .await?;
    assert_eq!(
        delivered.external_trigger_id,
        capability.external_trigger_id
    );
    let next = runtime.reset_external_trigger().await?;
    assert_ne!(next.external_trigger_id, capability.external_trigger_id);
    runtime
        .revoke_external_trigger(&next.external_trigger_id)
        .await?;
    let kinds = drain(runtime, &mut rx)?;
    assert!(kinds.iter().any(|kind| kind == "callback_delivered"));
    assert!(kinds.iter().any(|kind| kind.starts_with("wake_hint_")));
    assert!(kinds.iter().any(|kind| kind == "external_trigger_created"));
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| *kind == "external_trigger_revoked")
            .count(),
        2
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn independent_audit_writer_timer_keeps_original_ack_and_notification_order() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut rx = subscribe(runtime)?;
    let timer = runtime
        .schedule_timer(86_400_000, None, Some("original timer".into()))
        .await?;
    let cancelled = runtime.cancel_timer(&timer.id).await?;
    assert_eq!(cancelled.status, TimerStatus::Cancelled);
    assert_eq!(
        runtime
            .runtime_db()
            .timers()
            .latest(&timer.id)?
            .unwrap()
            .status,
        TimerStatus::Cancelled,
    );
    let kinds = drain(runtime, &mut rx)?;
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| matches!(kind.as_str(), "timer_created" | "timer_cancelled"))
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["timer_created", "timer_cancelled"],
    );
    Ok(())
}

#[tokio::test]
async fn independent_audit_writer_operator_failure_preserves_prior_notification_record(
) -> Result<()> {
    for reject in [false, true] {
        let harness = LifecycleHarness::new();
        let runtime = harness.runtime();
        runtime.await_initial_resources().await;
        let _refresh = runtime.hold_resource_refresh_for_test().await?;
        let mut rx = subscribe(runtime)?;
        if reject {
            runtime.runtime_db().transaction(|tx| {
                tx.execute_batch(
                    "CREATE TRIGGER reject_operator_audit BEFORE INSERT ON audit_events
                     WHEN NEW.kind='operator_notification_requested'
                     BEGIN SELECT RAISE(ABORT,'original operator Audit rejected'); END;",
                )?;
                Ok(())
            })?;
        }
        let result = runtime
            .notify_operator("original operator notification".into())
            .await;
        let records = runtime.storage().read_recent_operator_notifications(10)?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].message, "original operator notification");
        let kinds = drain(runtime, &mut rx)?;
        if reject {
            assert!(
                format!("{:#}", result.unwrap_err()).contains("original operator Audit rejected")
            );
            assert!(kinds.is_empty());
        } else {
            assert_eq!(result?.notification_id, records[0].notification_id);
            assert!(kinds
                .iter()
                .any(|kind| kind == "operator_notification_requested"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn independent_audit_writer_scheduler_pair_is_atomic_qualified_and_deduplicated() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut rx = subscribe(runtime)?;
    let decision = scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::ReduceMessageOnly,
        "original scheduler pair",
    );
    assert!(runtime.append_scheduler_decision(&decision).await?);
    let kinds = drain(runtime, &mut rx)?;
    assert_eq!(kinds.len(), 2);
    assert_eq!(kinds[1], "scheduler_decision");
    assert!(!runtime.append_scheduler_decision(&decision).await?);
    assert!(drain(runtime, &mut rx)?.is_empty());
    let before = runtime.storage().read_recent_events(32)?;
    let state = runtime.agent_state().await?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_second_scheduler_event BEFORE INSERT ON audit_events
             WHEN NEW.kind='scheduler_decision'
             BEGIN SELECT RAISE(ABORT,'second scheduler event rejected'); END;",
        )?;
        Ok(())
    })?;
    let next = scheduler::SchedulerDecision::new(
        scheduler::SchedulerDecisionKind::ReduceMessageOnly,
        "next independent pair",
    );
    let error = runtime.append_scheduler_decision(&next).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("second scheduler event rejected"),
        "{error:#}"
    );
    assert_eq!(runtime.storage().read_recent_events(32)?, before);
    assert_eq!(runtime.agent_state().await?, state);
    assert!(drain(runtime, &mut rx)?.is_empty());
    Ok(())
}

#[tokio::test]
async fn independent_audit_writer_scheduler_advisories_reuse_owner_and_keep_individual_transactions(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut rx = subscribe(runtime)?;
    let diagnostic = scheduler::SchedulingAdvisory {
        kind: "original-advisory".into(),
        severity: scheduler::SchedulingAdvisorySeverity::Warning,
        message: "original independent advisory".into(),
        work_item_id: None,
        wait_condition_id: None,
        evidence: vec!["original-evidence".into()],
    };
    let mut rejected = diagnostic.clone();
    rejected.kind = "rejected-advisory".into();
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_second_advisory BEFORE INSERT ON audit_events
             WHEN NEW.kind='scheduling_advisory'
               AND json_extract(NEW.data_json,'$.data.kind')='rejected-advisory'
             BEGIN SELECT RAISE(ABORT,'second advisory rejected'); END;",
        )?;
        Ok(())
    })?;
    let mut owner = runtime.begin_mutation().await?;
    let diagnostics = [diagnostic.clone(), diagnostic.clone(), rejected];
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        owner.append_prepared_scheduling_advisories(&diagnostics),
    )
    .await?
    .unwrap_err();
    assert!(
        format!("{error:#}").contains("second advisory rejected"),
        "{error:#}"
    );
    drop(owner);
    assert_eq!(drain(runtime, &mut rx)?, ["scheduling_advisory"]);
    assert_eq!(
        runtime
            .append_prepared_scheduling_advisories(&[diagnostic])
            .await?,
        0
    );
    assert!(drain(runtime, &mut rx)?.is_empty());
    Ok(())
}
