use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::{admit_cloned_value, ResourceReadMaterial};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "selector-agent".into(),
        identity_incarnation: 1,
    }
}

fn header(scope: &InputEvidenceScope) -> (u64, usize) {
    ((scope.agent_id.len() as u64 + 256) * 8, 64)
}

// Independent JSON byte lengths and structural slots, not constructor sampling.
fn cost(bytes: u64, slots: usize) -> (u64, usize) {
    ((bytes * 4 + slots as u64 * 128 + 512) * 8, (slots + 16) * 8)
}

#[test]
fn current_queue_selector_includes_cancelled_pending_wake_without_a_new_birth() -> Result<()> {
    use crate::{
        runtime_db::TimerWakeStatus,
        types::{TimerRecord, TimerStatus},
    };
    let scope = scope();
    let at = "2026-10-21T00:00:00Z".parse()?;
    let timer = TimerRecord {
        calendar: None,
        id: "timer".into(),
        agent_id: scope.agent_id,
        created_at: at,
        duration_ms: 1000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(at),
        last_fired_at: Some(at),
        fire_count: 1,
    };
    let cancelled = TimerRecord {
        status: TimerStatus::Cancelled,
        next_fire_at: None,
        ..timer.clone()
    };
    let wake = TimerWakeRecord {
        timer_id: timer.id.clone(),
        message_id: "pending-路".into(),
        fire_count: 1,
        status: TimerWakeStatus::Pending,
        created_at: at,
        updated_at: at,
        incorporated_at: None,
        cancelled_at: None,
    };
    for pending in [None, Some(wake)] {
        let expected = pending.as_ref().map(|wake| wake.message_id.clone());
        let command = PreparedTimerTransition::cancel(
            TimerCancel {
                cancelled_at: chrono::Utc::now(),
                expected: timer.clone(),
                record: cancelled.clone(),
                agent_state: None,
            },
            pending,
            None,
        );
        assert_eq!(command.queue_write_address(), expected.as_deref());
        assert!(command.new_wake().is_none());
        if let PreparedTimerTransition::Cancel {
            pending: Some(wake),
            ..
        } = &command
        {
            assert_eq!(
                command.queue_write_address().unwrap().as_ptr(),
                wake.message_id.as_ptr()
            );
        }
    }
    assert!(PreparedTimerTransition::create(timer, None)
        .queue_write_address()
        .is_none());
    Ok(())
}

#[test]
fn selector_copies_share_exact_used_remainder() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let header = header(&scope);
    let timer_cost = cost(7, 1); // "timer"
    let copy_cost = cost(br#"{"family":"timer","id":"timer"}"#.len() as u64, 9);
    let wake_cost = cost(16, 5); // ["timer","wake"]
    let exact = (
        header.0 + timer_cost.0 + copy_cost.0 + 2 * wake_cost.0,
        header.1 + timer_cost.1 + copy_cost.1 + 2 * wake_cost.1,
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let timer = timer_selector("timer")?;
        admit_cloned_value(&timer)?;
        let mut keys = BTreeSet::from([timer.clone()]);
        keys.insert(wake_selector("timer", "wake")?);
        let lookup = wake_selector("timer", "wake")?;
        assert!(keys.contains(&lookup));
        assert_eq!(keys.len(), 2);
        assert_eq!(phase.finish()?, exact);
        let observed = ledger.report()?.observed_bytes;
        assert!(wake_selector("timer", "wake").is_err());
        assert!(phase.finish().is_err());
        assert_eq!(ledger.report()?.observed_bytes, observed);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn each_selector_refuses_before_allocation_without_resetting_failed_phase() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let header = header(&scope);
    for wake in [false, true] {
        let cost = if wake { cost(16, 5) } else { cost(7, 1) };
        for bytes_bound in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                if bytes_bound {
                    header.0 + cost.0 - 1
                } else {
                    1024 * 1024
                },
                if bytes_bound {
                    8192
                } else {
                    header.1 + cost.1 - 1
                },
            )?;
            {
                let _entry = phase.enter();
                let observed = ledger.report()?.observed_bytes;
                for _ in 0..2 {
                    let error = if wake {
                        wake_selector("timer", "wake")
                    } else {
                        timer_selector("timer")
                    }
                    .unwrap_err();
                    assert!(
                        format!("{error:#}").contains("ResourceExhausted"),
                        "{error:#}"
                    );
                    assert!(phase.finish().is_err());
                    assert_eq!(ledger.report()?.observed_bytes, observed);
                    assert!(ResourceReadMaterial::current_or_new(&ledger, &scope).is_err());
                }
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn wake_selector_preserves_canonical_escaping_and_tls() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let phase = ResourceReadMaterial::current_or_new(&ledger, &scope)?;
    {
        let _entry = phase.enter();
        let key = wake_selector("t\"\\\n☃", "m\t/")?;
        assert_eq!(key.family, SchedulingFamily::TimerWake);
        assert_eq!(key.id, r#"["t\"\\\n☃","m\t/"]"#);
        assert_eq!(key.wake_pair()?, ("t\"\\\n☃".into(), "m\t/".into()));
        assert_eq!(timer_selector("t\"\\\n☃")?.id, "t\"\\\n☃");
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
