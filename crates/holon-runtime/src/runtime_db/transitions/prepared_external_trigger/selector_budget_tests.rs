use super::*;
use crate::config::StateMemoryLimits;
use crate::diagnostics::resource_io::{admit_cloned_value, ResourceReadMaterial};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "trigger-selector-agent".into(),
        identity_incarnation: 1,
    }
}
// Independent JSON lengths and structural slots; no constructor/usage sampling.
fn cost(bytes: u64, slots: usize) -> (u64, usize) {
    ((bytes * 4 + slots as u64 * 128 + 512) * 8, (slots + 16) * 8)
}

#[test]
fn observation_reserved_lookup_and_effects_share_exact_selector_allowance() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
    let selector = cost(9, 1); // "trigger"
    let set_copy = cost(
        br#"{"family":"external_trigger","id":"trigger"}"#.len() as u64,
        9,
    );
    let exact = (
        header.0 + 3 * selector.0 + set_copy.0,
        header.1 + 3 * selector.1 + set_copy.1,
    );
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, exact.0, exact.1)?;
    {
        let _entry = phase.enter();
        let key = trigger_selector("trigger")?;
        admit_cloned_value(&key)?;
        let keys = BTreeSet::from([key.clone()]);
        assert!(keys.contains(&trigger_selector("trigger")?));
        assert_eq!(trigger_selector("trigger")?, key);
        assert_eq!(phase.finish()?, exact);
        let held = ledger.report()?.observed_bytes;
        assert!(trigger_selector("trigger").is_err());
        assert!(phase.finish().is_err());
        assert_eq!(ledger.report()?.observed_bytes, held);
    }
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn selector_gate_refuses_each_bound_and_preserves_canonical_raw_id() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = scope();
    let header = ((scope.agent_id.len() as u64 + 256) * 8, 64);
    let selector = cost(9, 1);
    for bytes_bound in [false, true] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            if bytes_bound {
                header.0 + selector.0 - 1
            } else {
                1024 * 1024
            },
            if bytes_bound {
                8192
            } else {
                header.1 + selector.1 - 1
            },
        )?;
        {
            let _entry = phase.enter();
            let held = ledger.report()?.observed_bytes;
            for _ in 0..2 {
                let error = trigger_selector("trigger").unwrap_err();
                assert!(
                    format!("{error:#}").contains("ResourceExhausted"),
                    "{error:#}"
                );
                assert!(ResourceReadMaterial::current_or_new(&ledger, &scope).is_err());
                assert!(phase.finish().is_err());
                assert_eq!(ledger.report()?.observed_bytes, held);
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    let phase = ResourceReadMaterial::new(&ledger, &scope)?;
    {
        let _entry = phase.enter();
        let id = "trigger\"\\\n界";
        assert_eq!(
            trigger_selector(id)?,
            SchedulingKey::new(SchedulingFamily::ExternalTrigger, id)
        );
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
