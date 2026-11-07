use super::*;
use crate::diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial};
use crate::runtime_db::scheduling_snapshot::tests::{ledger, scope};

// Independent scalar/key JSON envelopes, not an admission measurement.
fn cost(bytes: usize, slots: usize, values: usize) -> (u64, usize) {
    (
        (bytes as u64 * 4 + slots as u64 * 128 + 512) * 8,
        values * 8,
    )
}
fn sum(parts: &[(u64, usize)]) -> (u64, usize) {
    parts.iter().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1))
}
#[test]
fn repeated_wake_parents_admit_only_actual_owning_insertions() -> Result<()> {
    let parent = SchedulingKey::new(SchedulingFamily::Timer, "parent\"雪");
    let string = cost(serde_json::to_vec(&parent.id)?.len(), 1, 1);
    let key = cost(serde_json::to_vec(&parent)?.len(), 9, 3);
    for (in_core, in_selected) in [(true, true), (true, false), (false, true), (false, false)] {
        let parts: Vec<_> = match (in_core, in_selected) {
            (true, true) => vec![],
            (true, false) => vec![string, key],
            (false, true) => vec![string],
            (false, false) => vec![string, key, key],
        };
        let ledger = ledger()?;
        let limits = (16 * 1024 * 1024, 4096);
        for next in 0..parts.len() {
            for bytes_bound in [false, true] {
                let mut core = BTreeSet::new();
                let mut selected = BTreeSet::new();
                let mut pending = Vec::new();
                if in_core {
                    core.insert(parent.clone());
                }
                if in_selected {
                    selected.insert(parent.clone());
                }
                let phase =
                    ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
                let baseline = phase.finish()?;
                let before = sum(&parts[..next]);
                {
                    let _entered = phase.enter();
                    if bytes_bound {
                        charge_owned_metadata(
                            (limits.0 - baseline.0 - before.0 - parts[next].0) / 8 + 1,
                            0,
                        )?;
                    } else {
                        charge_owned_metadata(
                            0,
                            (limits.1 - baseline.1 - before.1 - parts[next].1) / 8 + 1,
                        )?;
                    }
                    let error =
                        include_wake_parent(&mut core, &mut selected, &mut pending, &parent.id)
                            .unwrap_err();
                    assert!(error.to_string().contains("ResourceExhausted"));
                }
                assert!(phase.finish().is_err());
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
        let mut core = BTreeSet::new();
        let mut selected = BTreeSet::new();
        let mut pending = Vec::new();
        if in_core {
            core.insert(parent.clone());
        }
        if in_selected {
            selected.insert(parent.clone());
        }
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), limits.0, limits.1)?;
        let baseline = phase.finish()?;
        {
            let _entered = phase.enter();
            for _ in 0..256 {
                include_wake_parent(&mut core, &mut selected, &mut pending, &parent.id)?;
            }
        }
        let expected = sum(&parts);
        assert_eq!(
            phase.finish()?,
            (baseline.0 + expected.0, baseline.1 + expected.1)
        );
        assert_eq!(core, BTreeSet::from([parent.clone()]));
        assert_eq!(selected, BTreeSet::from([parent.clone()]));
        assert_eq!(pending.len(), usize::from(!in_selected));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
