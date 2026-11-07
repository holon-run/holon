use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

type Cost = (u64, usize);
fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "scheduling-agent".into(),
        identity_incarnation: 1,
    }
}
fn header() -> Cost {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}
fn add(a: Cost, b: Cost) -> Cost {
    (a.0 + b.0, a.1 + b.1)
}
fn insertion(id: &str) -> Cost {
    (((id.len() + std::mem::size_of::<String>()) * 8) as u64, 8)
}
fn nodes(count: usize) -> Cost {
    (
        (count * std::mem::size_of::<String>() * 8) as u64,
        count * 8,
    )
}
fn ledger() -> Result<InputEvidenceBudgetLedger> {
    Ok(InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?)
}
fn exhausted(error: anyhow::Error) {
    assert!(format!("{error:#}")
        .to_lowercase()
        .contains("resourceexhausted"));
}
fn writes(primary_applied: bool, waits: &[&str], turns: &[&str]) -> WaitConditionWrites {
    WaitConditionWrites {
        primary_applied,
        wait_ids: waits.iter().map(|s| (*s).to_owned()).collect(),
        turn_ids: turns.iter().map(|s| (*s).to_owned()).collect(),
    }
}

#[test]
fn insertion_exact_cost_duplicates_and_last_transaction_holder() -> Result<()> {
    let wait = "wait-多字节";
    let turn = "turn-owner";
    let body = add(insertion(wait), insertion(turn));
    let exact = add(add(header(), header()), body);
    let budget = ledger()?;
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let mut actual = WaitConditionWrites::default();
    {
        let _entry = transaction.enter();
        WaitConditionWrites::insert_id(&mut actual.wait_ids, wait)?;
        WaitConditionWrites::insert_id(&mut actual.turn_ids, turn)?;
        let wait_pointer = actual.wait_ids.first().unwrap().as_ptr();
        let turn_pointer = actual.turn_ids.first().unwrap().as_ptr();
        // Exact remaining allowance is zero: duplicates must not allocate.
        WaitConditionWrites::insert_id(&mut actual.wait_ids, wait)?;
        WaitConditionWrites::insert_id(&mut actual.turn_ids, turn)?;
        assert_eq!(actual.wait_ids.first().unwrap().as_ptr(), wait_pointer);
        assert_eq!(actual.turn_ids.first().unwrap().as_ptr(), turn_pointer);
    }
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), body));
    let holder = std::sync::Arc::new((actual, transaction.guard()?));
    let last = holder.clone();
    drop((phase, transaction, holder));
    assert_eq!(last.0.wait_ids.first().unwrap(), wait);
    assert_eq!(last.0.turn_ids.first().unwrap(), turn);
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn each_insertion_gate_uses_original_remainder_and_restores_tls() -> Result<()> {
    let ids = ["wait-original", "turn-original"];
    let mut prefix = add(header(), header());
    for (index, id) in ids.iter().enumerate() {
        let next = insertion(id);
        for bytes in [true, false] {
            let budget = ledger()?;
            let phase = ResourceReadMaterial::with_test_limits(
                &budget,
                &scope(),
                if bytes {
                    prefix.0 + next.0 - 1
                } else {
                    64 * 1024 * 1024
                },
                if bytes { 65536 } else { prefix.1 + next.1 - 1 },
            )?;
            let transaction = phase.transaction()?;
            let outside_budget = ledger()?;
            let outside = ResourceReadMaterial::new(&outside_budget, &scope())?;
            let mut actual = WaitConditionWrites::default();
            {
                let _outside = outside.enter();
                {
                    let _entry = transaction.enter();
                    if index == 1 {
                        WaitConditionWrites::insert_id(&mut actual.wait_ids, ids[0])?;
                    }
                    let destination = if index == 0 {
                        &mut actual.wait_ids
                    } else {
                        &mut actual.turn_ids
                    };
                    exhausted(WaitConditionWrites::insert_id(destination, id).unwrap_err());
                    assert!(destination.is_empty());
                    exhausted(WaitConditionWrites::insert_id(destination, "retry").unwrap_err());
                }
                charge_owned_metadata(7, 1)?;
            }
            assert_eq!(outside.finish()?, add(header(), (56, 8)));
            assert_eq!(budget.report()?.observed_bytes, prefix.0);
            assert_eq!(actual.wait_ids.len(), index);
            assert!(!actual.primary_applied);
            assert!(transaction.finish().is_err());
            drop((transaction, actual));
            assert_eq!(budget.report()?.observed_bytes, header().0);
            assert!(phase.finish().is_err());
            assert!(phase.transaction().is_err());
            drop(phase);
            assert_eq!(budget.report()?.observed_bytes, 0);
            drop(outside);
            assert_eq!(outside_budget.report()?.observed_bytes, 0);
        }
        prefix = add(prefix, next);
    }
    Ok(())
}

#[test]
fn move_merge_charges_only_unique_slots_and_keeps_original_strings() -> Result<()> {
    let budget = ledger()?;
    let body = nodes(2);
    let exact = add(add(header(), header()), body);
    let phase = ResourceReadMaterial::with_test_limits(&budget, &scope(), exact.0, exact.1)?;
    let transaction = phase.transaction()?;
    let mut destination = writes(false, &["existing"], &["existing"]);
    let source = writes(true, &["existing", "new-wait"], &["existing", "new-turn"]);
    let wait_pointer = source.wait_ids.get("new-wait").unwrap().as_ptr();
    let turn_pointer = source.turn_ids.get("new-turn").unwrap().as_ptr();
    let existing_pointer = destination.wait_ids.get("existing").unwrap().as_ptr();
    {
        let _entry = transaction.enter();
        destination.extend(source)?;
        assert!(destination.primary_applied);
        assert_eq!(
            destination.wait_ids.get("new-wait").unwrap().as_ptr(),
            wait_pointer
        );
        assert_eq!(
            destination.turn_ids.get("new-turn").unwrap().as_ptr(),
            turn_pointer
        );
        assert_eq!(
            destination.wait_ids.get("existing").unwrap().as_ptr(),
            existing_pointer
        );
        // Empty and duplicate merges remain free at the exact ceiling.
        destination.extend(WaitConditionWrites::default())?;
        destination.extend(writes(false, &["existing"], &["existing"]))?;
    }
    assert_eq!(phase.finish()?, exact);
    assert_eq!(transaction.finish()?, add(header(), body));
    assert_eq!(destination.wait_ids.len(), 2);
    assert_eq!(destination.turn_ids.len(), 2);
    let first = std::sync::Arc::new((destination, transaction.guard()?));
    let last = first.clone();
    drop((first, transaction, phase));
    assert_eq!(budget.report()?.observed_bytes, add(header(), body).0);
    drop(last);
    assert_eq!(budget.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn merge_denial_has_no_partial_sets_or_primary_outcome() -> Result<()> {
    let used = insertion("prior-material");
    let prefix = add(add(header(), header()), used);
    let next = nodes(2);
    for bytes in [true, false] {
        let budget = ledger()?;
        let phase = ResourceReadMaterial::with_test_limits(
            &budget,
            &scope(),
            if bytes {
                prefix.0 + next.0 - 1
            } else {
                64 * 1024 * 1024
            },
            if bytes { 65536 } else { prefix.1 + next.1 - 1 },
        )?;
        let transaction = phase.transaction()?;
        let mut destination = writes(false, &["existing"], &["existing"]);
        let before_wait = destination.wait_ids.get("existing").unwrap().as_ptr();
        let before_turn = destination.turn_ids.get("existing").unwrap().as_ptr();
        {
            let _entry = transaction.enter();
            let mut prior = BTreeSet::new();
            WaitConditionWrites::insert_id(&mut prior, "prior-material")?;
            exhausted(
                destination
                    .extend(writes(
                        true,
                        &["existing", "new-wait"],
                        &["existing", "new-turn"],
                    ))
                    .unwrap_err(),
            );
            assert_eq!(destination.wait_ids, BTreeSet::from(["existing".into()]));
            assert_eq!(destination.turn_ids, BTreeSet::from(["existing".into()]));
            assert_eq!(destination.wait_ids.first().unwrap().as_ptr(), before_wait);
            assert_eq!(destination.turn_ids.first().unwrap().as_ptr(), before_turn);
            assert!(!destination.primary_applied);
            exhausted(
                destination
                    .extend(writes(true, &["retry"], &[]))
                    .unwrap_err(),
            );
        }
        assert_eq!(budget.report()?.observed_bytes, prefix.0);
        assert!(transaction.finish().is_err());
        drop((transaction, destination));
        assert_eq!(budget.report()?.observed_bytes, header().0);
        assert!(phase.finish().is_err());
        drop(phase);
        assert_eq!(budget.report()?.observed_bytes, 0);
    }
    Ok(())
}
