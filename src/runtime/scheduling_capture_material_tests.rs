use super::*;
use crate::runtime::tests::support::LifecycleHarness;
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
async fn stop_loop(runtime: &crate::runtime::RuntimeHandle, id: &str) -> Result<()> {
    for lease in runtime.inner.control_cancellations.timer_for(id) {
        lease.cancel();
        lease.join().await?;
    }
    Ok(())
}
#[tokio::test]
async fn actual_scheduling_joint_capture_preserves_wake_closure_without_sql_or_unselected_body(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let large = runtime
        .schedule_timer(3_600_000, None, Some("large".repeat(100 * 1024)))
        .await?;
    stop_loop(runtime, &large.id).await?;
    let mut timer = runtime
        .schedule_timer(
            3_600_000,
            Some(3_600_000),
            Some("actual selected wake".into()),
        )
        .await?;
    stop_loop(runtime, &timer.id).await?;
    runtime.fire_timer_record(&mut timer).await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Scheduling root missing")?;
    let domain = root
        .scheduling
        .as_ref()
        .context("actual Scheduling domain missing")?;
    let wake = domain
        .records
        .values()
        .find(|r| matches!(&r.fact,CanonicalSchedulingFact::TimerWake(w) if w.timer_id==timer.id))
        .context("actual Wake missing")?;
    let request = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([wake.locator.key.clone()]),
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE timers RENAME TO joint_scheduling_forbidden_timers;
         ALTER TABLE timer_wakes RENAME TO joint_scheduling_forbidden_wakes;
         ALTER TABLE external_triggers RENAME TO joint_scheduling_forbidden_triggers;
         ALTER TABLE agent_states RENAME TO joint_scheduling_forbidden_states;",
    )?;
    let (captured, hold) = domain.capture_with_material(&request, ledger, LIMITS, (0, 0))?;
    assert_eq!(captured.records.len(), 2);
    assert_eq!(captured.timer(&timer.id)?, Some(&timer));
    assert!(captured.records.iter().any(|r| Arc::ptr_eq(r, wake)));
    let timer_key = SchedulingKey::new(SchedulingFamily::Timer, &timer.id);
    assert!(captured
        .records
        .iter()
        .any(|r| Arc::ptr_eq(r, domain.records.get(&timer_key).unwrap())));
    assert!(captured
        .records
        .iter()
        .all(|r| r.locator.key.id != large.id));
    assert!(
        hold.phase_peak().0 < u64::try_from(large.summary.as_ref().unwrap().len())?,
        "unselected Timer body entered the allowance"
    );
    hold.validate_accounting(domain.scope(), ledger)?;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(hold.validate_accounting(domain.scope(), &foreign).is_err());
    assert!(domain
        .capture_with_material(&request, &foreign, LIMITS, (0, 0))
        .is_err());
    assert_eq!(foreign.report()?.observed_bytes, 0);
    assert_eq!(
        Arc::strong_count(&root),
        strong,
        "capture retained its publication"
    );
    let weak = Arc::downgrade(hold._guards.last().unwrap());
    drop(captured);
    assert!(weak.upgrade().is_some());
    drop(hold);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let unknown = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(
            SchedulingFamily::Timer,
            "never-accepted",
        )]),
    };
    let error = domain
        .capture_with_material(&unknown, ledger, LIMITS, (0, 0))
        .unwrap_err();
    assert!(
        matches!(error.downcast_ref::<SchedulingReadError>(),Some(SchedulingReadError::NeedsPreparation(key)) if key.id=="never-accepted")
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn actual_scheduling_caller_prior_is_admitted_before_clones_and_scratch_handoff() -> Result<()>
{
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("joint selected".into()))
        .await?;
    stop_loop(runtime, &timer.id).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let domain = root.scheduling.as_ref().unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let request = SchedulingCaptureRequest {
        include_live: false,
        history_limit: 0,
        required_keys: BTreeSet::from([SchedulingKey::new(SchedulingFamily::Timer, &timer.id)]),
    };
    let (captured, hold) = domain.capture_with_material(&request, ledger, LIMITS, (0, 0))?;
    let peak = hold.phase_peak();
    drop((captured, hold));
    let bytes = vec![3u8; 128 * 1024];
    let prior = (u64::try_from(bytes.len())?, 16usize);
    let guard = ledger.register_owned_bytes(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let held = ledger.report()?.observed_bytes;
    let exact = (usize::try_from(prior.0 + peak.0)?, prior.1 + peak.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let error = domain
            .capture_with_material(&request, ledger, limits, prior)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<SchedulingReadError>(),
            Some(SchedulingReadError::ResourceExhausted(
                "Scheduling capture material"
            ))
        ));
        assert_eq!(ledger.report()?.observed_bytes, held);
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
    }
    let (captured, hold) = domain.capture_with_material(&request, ledger, exact, prior)?;
    assert_eq!(hold.phase_peak(), peak);
    assert!(hold.material_estimate()?.0 < prior.0);
    drop((captured, hold));
    assert_eq!(ledger.report()?.observed_bytes, held);
    let mut phase = SchedulingCapturePhase::new(domain, ledger, LIMITS, prior)?;
    let captured = domain.capture_observed(&request, &mut phase)?;
    let scratch = phase.owned.as_ref().unwrap().guard();
    assert!(phase
        .finish(captured)
        .unwrap_err()
        .to_string()
        .contains("another live holder"));
    assert!(ledger.report()?.observed_bytes > held);
    drop(scratch);
    assert_eq!(ledger.report()?.observed_bytes, held);
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let mut phase = SchedulingCapturePhase::new(domain, ledger, LIMITS, prior)?;
    let captured = domain.capture_observed(&request, &mut phase)?;
    phase.owned = Some(foreign.reserve_owned_completion(
        domain.scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: 0,
            metadata_estimate_bytes: phase.owned_bytes,
        },
    )?);
    assert!(phase
        .finish(captured)
        .unwrap_err()
        .to_string()
        .contains("another phase"));
    assert_eq!(foreign.report()?.observed_bytes, 0);
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop((guard, bytes));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[test]
fn borrowed_scheduling_key_observer_matches_actual_key_object() -> Result<()> {
    for family in [
        SchedulingFamily::Timer,
        SchedulingFamily::TimerWake,
        SchedulingFamily::ExternalTrigger,
    ] {
        for id in ["", "selected", "quote\"\\\n雪"] {
            let key = SchedulingKey::new(family, id);
            let borrowed = BorrowedSchedulingKey { family, id };
            assert_eq!(serde_json::to_vec(&key)?, serde_json::to_vec(&borrowed)?);
            assert_eq!(json_cost(&key, 1)?, json_cost(&borrowed, 1)?);
        }
    }
    Ok(())
}
