use super::*;
use crate::types::{CallbackDeliveryMode, ExternalTriggerScope, ExternalTriggerStatus};

fn triggers(count: usize, text: &str) -> Vec<ExternalTriggerRecord> {
    let at = chrono::DateTime::from_timestamp_millis(2_000).unwrap();
    (0..count)
        .map(|index| ExternalTriggerRecord {
            external_trigger_id: format!("duplicate-{}-{text}", index % 2),
            target_agent_id: scope().agent_id,
            scope: ExternalTriggerScope::Agent,
            delivery_mode: if index % 2 == 0 {
                CallbackDeliveryMode::WakeHint
            } else {
                CallbackDeliveryMode::EnqueueMessage
            },
            token: (index % 2 == 0).then(|| format!("fixture-only-{text}")),
            token_hash: format!("fixture-hash-{text}"),
            status: if index % 3 == 0 {
                ExternalTriggerStatus::Revoked
            } else {
                ExternalTriggerStatus::Active
            },
            created_at: at,
            revoked_at: (index % 3 == 0).then_some(at),
            last_delivered_at: (index % 2 == 0).then_some(at),
            delivery_count: index as u64,
        })
        .collect()
}

fn view(queue: &crate::work_item_scheduling::WorkQueueReadModel) -> serde_json::Value {
    serde_json::json!({
        "current": &queue.current,
        "queued_blocked": &queue.queued_blocked,
        "items": &queue.items,
        "current_runnable": &queue.current_runnable,
        "triggered_blocked": &queue.triggered_blocked,
        "queued_runnable": &queue.queued_runnable,
        "yielded": &queue.yielded,
        "waiting_for_operator": &queue.waiting_for_operator,
        "blocked": &queue.blocked,
        "completing": &queue.completing,
        "completed_recent": &queue.completed_recent,
    })
}

#[test]
fn trigger_vec_copy_has_exact_independent_shape_cost_and_complete_independent_fields() -> Result<()>
{
    for text in ["", "汉字🦀\0\n\t\"{}[],:\\", &"long中".repeat(1024)] {
        for count in [0, 1, 4, 5, 17] {
            let source = triggers(count, text);
            let cost = if source.is_empty() {
                (0, 0)
            } else {
                clone_cost(&source)?
            };
            let total = add(header(), cost);
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
            let holder = phase.guard()?;
            let copy = {
                let _entry = phase.enter();
                clone_context_queue_triggers(&source)?
            };
            assert_eq!(copy, source);
            assert_eq!(copy.capacity(), count);
            if count > 0 {
                assert_ne!(copy.as_ptr(), source.as_ptr());
                for (copy, source) in copy.iter().zip(&source) {
                    assert_ne!(
                        copy.external_trigger_id.as_ptr(),
                        source.external_trigger_id.as_ptr()
                    );
                    assert_ne!(
                        copy.target_agent_id.as_ptr(),
                        source.target_agent_id.as_ptr()
                    );
                    assert_ne!(copy.token_hash.as_ptr(), source.token_hash.as_ptr());
                    if let (Some(copy), Some(source)) = (&copy.token, &source.token) {
                        assert_ne!(copy.as_ptr(), source.as_ptr());
                    }
                }
            }
            assert_eq!(phase.finish()?, total);
            assert!(Arc::ptr_eq(&holder, &phase.guard()?));
            drop((copy, source, phase));
            assert_eq!(ledger.report()?.observed_bytes, total.0);
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn context_trigger_copy_rejects_both_axes_before_projection_and_latches_original_phase(
) -> Result<()> {
    let captured = capture()?;
    let source = triggers(5, "临界\0\"{}[]");
    let original = source.clone();
    let pointer = source.as_ptr();
    let total = add(header(), clone_cost(&source)?);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            total.0 - u64::from(bytes),
            total.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        for _ in 0..2 {
            let error = captured
                .work_queue_with_material(&AgentState::new(scope().agent_id), &source, &phase)
                .unwrap_err();
            assert_eq!(error.to_string(), "context queue trigger clone material");
            let io = error.downcast_ref::<std::io::Error>().unwrap();
            assert_eq!(io.kind(), std::io::ErrorKind::Other);
            assert_eq!(
                io.to_string(),
                "ResourceExhausted: context resource read material"
            );
            let descriptor = crate::runtime_error::describe_runtime_error(&error);
            assert_eq!(
                descriptor.domain,
                crate::runtime_error::RuntimeErrorDomain::Io
            );
            assert_eq!(descriptor.code, "io_error");
            assert!(!descriptor.retryable);
            assert!(ResourceReadMaterial::active_guard()?.is_none());
            let latched = phase.finish().unwrap_err();
            let runtime = latched
                .downcast_ref::<crate::runtime_error::RuntimeError>()
                .unwrap();
            let descriptor = runtime.descriptor();
            assert_eq!(
                descriptor.domain,
                crate::runtime_error::RuntimeErrorDomain::Io
            );
            assert_eq!(descriptor.code, "context_resource_exhausted");
            assert!(descriptor.retryable);
            assert_eq!(ledger.report()?.observed_bytes, header().0);
            assert_eq!(source, original);
            assert_eq!(source.as_ptr(), pointer);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, header().0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn context_trigger_adapter_preserves_owning_projection_and_exact_delta_through_last_holder(
) -> Result<()> {
    let captured = capture()?;
    let source = triggers(5, "完整字段\0\"{}[]");
    let state = AgentState::new(scope().agent_id);
    let baseline_ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let baseline = ResourceReadMaterial::new(&baseline_ledger, &scope())?;
    let expected = {
        let _entry = baseline.enter();
        captured.work_queue(&state, source.clone())?
    };
    let total = add(baseline.finish()?, clone_cost(&source)?);
    drop(baseline);
    assert_eq!(baseline_ledger.report()?.observed_bytes, 0);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    let queue = captured.work_queue_with_material(&state, &source, &phase)?;
    assert_eq!(view(&queue), view(&expected));
    assert_eq!(phase.finish()?, total);
    assert!(Arc::ptr_eq(&holder, &phase.guard()?));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((phase, captured, source));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    assert_eq!(view(&queue), view(&expected));
    struct Fail;
    impl std::io::Write for Fail {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("consumer failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(Fail, &queue.items).is_err());
    drop(queue);
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn context_trigger_adapter_restores_outer_phase_on_success_quota_and_scope_errors() -> Result<()> {
    let captured = capture()?;
    let source = triggers(2, "嵌套");
    let outer_ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let inner_ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let outer = ResourceReadMaterial::new(&outer_ledger, &scope())?;
    let inner = ResourceReadMaterial::new(&inner_ledger, &scope())?;
    let limited =
        ResourceReadMaterial::with_test_limits(&inner_ledger, &scope(), header().0, header().1)?;
    let outer_holder = outer.guard()?;
    {
        let _entry = outer.enter();
        captured.work_queue_with_material(&AgentState::new(scope().agent_id), &source, &inner)?;
        let quota = captured
            .work_queue_with_material(&AgentState::new(scope().agent_id), &source, &limited)
            .unwrap_err();
        assert_eq!(quota.to_string(), "context queue trigger clone material");
        assert!(quota.downcast_ref::<std::io::Error>().is_some());
        assert!(limited.finish().is_err());
        let error = captured
            .work_queue_with_material(&AgentState::new("foreign"), &source, &inner)
            .unwrap_err();
        assert_eq!(error.to_string(), "Work projection State scope mismatch");
        assert!(Arc::ptr_eq(
            &outer_holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        assert_eq!(outer.finish()?, header());
        assert!(inner.finish()?.0 > header().0);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((inner, limited, outer, outer_holder));
    assert_eq!(outer_ledger.report()?.observed_bytes, 0);
    assert_eq!(inner_ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn repeated_trigger_copies_accumulate_without_resetting_the_original_allowance() -> Result<()> {
    let source = triggers(1, "累计");
    let cost = clone_cost(&source)?;
    let total = add(add(header(), cost), cost);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        for _ in 0..2 {
            assert_eq!(clone_context_queue_triggers(&source)?, source);
            assert!(Arc::ptr_eq(
                &holder,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
        }
        assert_eq!(phase.finish()?, total);
        let error = clone_context_queue_triggers(&source).unwrap_err();
        assert_eq!(error.to_string(), "context queue trigger clone material");
        assert!(error.downcast_ref::<std::io::Error>().is_some());
        assert!(phase.finish().is_err());
        assert_eq!(ledger.report()?.observed_bytes, total.0);
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
