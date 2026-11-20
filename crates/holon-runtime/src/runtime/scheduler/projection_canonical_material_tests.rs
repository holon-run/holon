use super::canonical_ownership_tests::{clone_oracle, execution, observations};
use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "agent".into(),
        identity_incarnation: 1,
    }
}

fn header() -> (u64, usize) {
    ((("agent".len() + 256) * 8) as u64, 64)
}

fn cost(count: usize) -> (u64, usize) {
    (
        header().0
            + (count * std::mem::size_of::<(String, CanonicalWorkExecutionState)>() * 8) as u64,
        header().1 + count * 8,
    )
}

#[test]
fn cold_canonical_slots_exact_cost_moves_text_and_preserves_mapping() -> Result<()> {
    for count in [0, 1, 3, 4, 7, 8, 15, 32] {
        let mut source = execution();
        let seed = source.work_items.clone();
        source.work_items.clear();
        for (index, (id, row)) in seed.iter().cycle().take(count).enumerate() {
            source
                .work_items
                .insert(format!("{index}-{id}"), row.clone());
        }
        let count = source.work_items.len();
        let addresses = source
            .work_items
            .iter()
            .map(|(id, row)| {
                (
                    id.clone(),
                    (id.as_ptr(), id.capacity()),
                    match &row.state {
                        WorkItemExecutionState::Waiting { wait, .. } => {
                            Some((wait.wait_id.as_ptr(), wait.wait_id.capacity()))
                        }
                        _ => None,
                    },
                )
            })
            .collect::<Vec<_>>();
        let expected = clone_oracle(Some(&source));
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let required = cost(count);
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), required.0, required.1)?;
        SchedulerProjection::admit_canonical_slots(Some(&source), &phase)?;
        assert_eq!(phase.finish()?, required);
        let projection = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&AgentState::new("agent")),
            0,
            Default::default(),
            observations(Some(source)),
            Utc::now(),
        );
        assert_eq!(projection.canonical_work_states, expected);
        let states = projection.canonical_work_states.as_ref().unwrap();
        assert_eq!(states.len(), count);
        assert!(states.capacity() >= count);
        for (id, key_address, wait_address) in addresses {
            let (key, state) = states.get_key_value(&id).unwrap();
            assert_eq!((key.as_ptr(), key.capacity()), key_address);
            if let Some(address) = wait_address {
                let CanonicalWorkExecutionState::Waiting { wait_id } = state else {
                    panic!("Waiting mapping changed");
                };
                assert_eq!((wait_id.as_ptr(), wait_id.capacity()), address);
            }
        }
        let first = std::sync::Arc::new((projection, phase.guard()?));
        let last = first.clone();
        drop((first, phase));
        assert_eq!(ledger.report()?.observed_bytes, required.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn cold_canonical_slots_each_limit_refuses_and_latches_explicit_phase() -> Result<()> {
    let source = execution();
    let required = cost(source.work_items.len());
    for short_bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let outer = ResourceReadMaterial::new(&ledger, &scope())?;
        let outer_before = outer.finish()?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            required.0 - u64::from(short_bytes),
            required.1 - usize::from(!short_bytes),
        )?;
        let _outer_entry = outer.enter();
        let before = ledger.report()?.observed_bytes;
        let error = SchedulerProjection::admit_canonical_slots(Some(&source), &phase).unwrap_err();
        assert!(
            error.downcast_ref::<std::io::Error>().is_some(),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, before);
        assert_eq!(outer.finish()?, outer_before);
        let latched = phase.finish().unwrap_err().to_string();
        assert_eq!(
            phase
                .guard()
                .err()
                .context("failed phase returned guard")?
                .to_string(),
            latched
        );
        let retry = SchedulerProjection::admit_canonical_slots(None, &phase).unwrap_err();
        assert_eq!(retry.to_string(), error.to_string());
        assert_eq!(ledger.report()?.observed_bytes, before);
        assert_eq!(
            clone_oracle(Some(&source)).unwrap().len(),
            source.work_items.len()
        );
        drop((_outer_entry, phase, outer));
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn cold_canonical_slots_absent_and_empty_keep_fail_closed_empty_map() -> Result<()> {
    let mut empty = execution();
    empty.work_items.clear();
    for source in [None, Some(empty)] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let required = header();
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), required.0, required.1)?;
        SchedulerProjection::admit_canonical_slots(source.as_ref(), &phase)?;
        assert_eq!(phase.finish()?, required);
        let projection = SchedulerProjection::from_observations(
            SchedulerAgentSnapshot::from_state(&AgentState::new("agent")),
            0,
            Default::default(),
            observations(source),
            Utc::now(),
        );
        assert!(projection
            .canonical_work_states
            .as_ref()
            .unwrap()
            .is_empty());
        assert_eq!(
            projection
                .canonical_work_states
                .as_ref()
                .unwrap()
                .capacity(),
            0
        );
        let item = WorkItemRecord::new("agent", "no authority", crate::types::WorkItemState::Open);
        assert!(!projection.execution_authorizes_autonomous(&item));
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
