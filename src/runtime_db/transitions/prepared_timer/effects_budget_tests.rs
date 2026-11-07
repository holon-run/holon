use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "effects-agent".into(),
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
fn effects_and_finalizer_resume_one_exact_used_allowance() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let header = header(&scope);
    let timer = cost(7, 1);
    let wake = cost(16, 5);
    let exact = (
        header.0 + timer.0 + wake.0 + 8,
        header.1 + timer.1 + wake.1 + 8,
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    let key = {
        let _entry = resume_material(&phase)?;
        timer_selector("timer")?
    };
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    assert_eq!(phase.finish()?, (header.0 + timer.0, header.1 + timer.1));
    let wake = {
        let _entry = resume_material(&phase)?;
        wake_selector("timer", "wake")?
    };
    let guard = {
        let _entry = resume_material(&phase)?;
        charge_owned_metadata(1, 1)?;
        ResourceReadMaterial::active_guard()?.unwrap()
    };
    assert_eq!(phase.finish()?, exact);
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    {
        let _entry = resume_material(&phase)?;
        assert!(charge_owned_metadata(1, 1).is_err());
    }
    assert!(resume_material(&phase).is_err());
    drop((key, wake, phase));
    assert_eq!(ledger.report()?.observed_bytes, exact.0);
    drop(guard);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn resumed_effects_refuse_each_key_before_construction_and_keep_failure() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let header = header(&scope);
    for wake in [false, true] {
        let key_cost = if wake { cost(16, 5) } else { cost(7, 1) };
        for bytes_bound in [false, true] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope,
                if bytes_bound {
                    header.0 + 8 + key_cost.0 - 1
                } else {
                    1024 * 1024
                },
                if bytes_bound {
                    8192
                } else {
                    header.1 + 8 + key_cost.1 - 1
                },
            )?;
            {
                let _entry = phase.enter();
                charge_owned_metadata(1, 1)?;
            }
            let observed = ledger.report()?.observed_bytes;
            {
                let _entry = resume_material(&phase)?;
                let result = if wake {
                    wake_selector("timer", "wake")
                } else {
                    timer_selector("timer")
                };
                assert!(format!("{:#}", result.unwrap_err()).contains("ResourceExhausted"));
            }
            for _ in 0..2 {
                assert!(resume_material(&phase).is_err());
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard()?.is_none());
                assert_eq!(ledger.report()?.observed_bytes, observed);
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn resuming_original_effect_phase_restores_unrelated_tls_on_error() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let original_header = header(&scope);
    let original = ResourceReadMaterial::with_test_limits(
        &ledger,
        &scope,
        original_header.0,
        original_header.1,
    )?;
    let outer = ResourceReadMaterial::current_or_new(&ledger, &scope)?;
    {
        let _outer = outer.enter();
        let outer_guard = ResourceReadMaterial::active_guard()?.unwrap();
        let result: Result<()> = (|| {
            let _original = resume_material(&original)?;
            timer_selector("timer")?;
            Ok(())
        })();
        assert!(result.is_err());
        assert!(original.finish().is_err());
        assert!(Arc::ptr_eq(
            &outer_guard,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        assert!(resume_material(&original).is_err());
        charge_owned_metadata(1, 1)?;
        assert_eq!(
            outer.finish()?,
            (original_header.0 + 8, original_header.1 + 8)
        );
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((original, outer));
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
