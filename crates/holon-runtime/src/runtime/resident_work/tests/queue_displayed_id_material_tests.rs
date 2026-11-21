use super::*;
use crate::{config::StateMemoryLimits, diagnostics::resource_io::ResourceReadMaterial};

fn header() -> (u64, usize) {
    ((scope().agent_id.len() as u64 + 256) * 8, 64)
}

fn slots(count: usize) -> (u64, usize) {
    ((count * std::mem::size_of::<&str>()) as u64 * 8, count * 8)
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

fn capture() -> Result<CapturedWork> {
    let records = vec![
        qualified("open-中🦀", WorkItemState::Open, 1),
        qualified("", WorkItemState::Open, 1),
        qualified("done", WorkItemState::Completed, 1),
        qualified("excluded", WorkItemState::Completed, 1),
    ];
    domain(records.clone(), coverage(&records, 1)).capture(&WorkCaptureRequest::current())
}

fn candidate_ids(captured: &CapturedWork) -> Vec<&str> {
    // Independent reference oracle for the old membership construction.
    let mut ids = Vec::new();
    for evidence in &captured.records {
        if let CanonicalWorkRecord::Work(work) = &evidence.record {
            if work.state == WorkItemState::Open {
                ids.push(work.id.as_str());
            }
        }
    }
    for key in &captured.nonopen.keys {
        ids.push(key.id.as_str());
    }
    ids
}

#[test]
fn displayed_ids_borrow_original_text_and_admit_prededup_reference_slots() -> Result<()> {
    let mut captured = capture()?;
    captured.nonopen.keys = ["done", "missing", "open-中🦀", "done", ""]
        .map(|id| WorkKey::new(WorkFamily::Work, id))
        .to_vec();
    // Text length and escapes do not change reference-slot accounting.
    captured.nonopen.keys.push(WorkKey::new(
        WorkFamily::Work,
        "long中🦀\0\"[]{}\\".repeat(4096),
    ));
    let originals = candidate_ids(&captured);
    let expected = originals.iter().copied().collect::<BTreeSet<_>>();
    let count = originals.len();
    let total = add(header(), slots(count));
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    let holder = phase.guard()?;
    let ids = {
        let _entry = phase.enter();
        let ids = queue_displayed_work_ids(&captured)?;
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        ids
    };
    assert_eq!(ids, expected.into_iter().collect::<Vec<_>>());
    assert_eq!(ids.capacity(), count);
    for id in &ids {
        assert!(originals
            .iter()
            .any(|original| original.as_ptr() == id.as_ptr() && original.len() == id.len()));
    }
    assert!(ids.binary_search(&"excluded").is_err());
    assert!(ids.binary_search(&"missing").is_ok());
    assert!(ids.binary_search(&"").is_ok());
    assert_eq!(phase.finish()?, total);
    drop((ids, phase));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn empty_displayed_ids_allocate_and_charge_no_reference_slots() -> Result<()> {
    let mut captured = capture()?;
    captured.records.clear();
    captured.nonopen.keys.clear();
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let total = header();
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), total.0, total.1)?;
    {
        let _entry = phase.enter();
        for _ in 0..2 {
            let ids = queue_displayed_work_ids(&captured)?;
            assert!(ids.is_empty());
            assert_eq!(ids.capacity(), 0);
        }
    }
    assert_eq!(phase.finish()?, total);
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn displayed_ids_reject_each_axis_cumulatively_and_latch_original_phase() -> Result<()> {
    let captured = capture()?;
    let cost = slots(candidate_ids(&captured).len());
    let prefix = add(header(), cost);
    let full = add(prefix, cost);
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            full.0 - u64::from(bytes),
            full.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let first = queue_displayed_work_ids(&captured)?;
            for _ in 0..2 {
                let error = queue_displayed_work_ids(&captured).unwrap_err();
                assert_eq!(error.to_string(), "resident displayed-ID slots admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(first, ["", "done", "excluded", "open-中🦀"]);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, prefix.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert_eq!(queue_displayed_work_ids(&captured)?.len(), 4);
    Ok(())
}

#[test]
fn resident_queue_rejects_displayed_ids_before_key_or_record_clones() -> Result<()> {
    let captured = capture()?;
    let state = AgentState::new(&scope().agent_id);
    let total = add(header(), slots(candidate_ids(&captured).len()));
    for bytes in [true, false] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(),
            total.0 - u64::from(bytes),
            total.1 - usize::from(!bytes),
        )?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            for _ in 0..2 {
                let error = captured.work_queue(&state, vec![]).unwrap_err();
                assert_eq!(error.to_string(), "resident displayed-ID slots admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, header().0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
            }
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, header().0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    assert_eq!(captured.records.len(), 4);
    assert_eq!(captured.work_queue(&state, vec![])?.items.len(), 4);
    Ok(())
}

#[test]
fn resident_displayed_membership_preserves_validation_queue_and_consumer_holder() -> Result<()> {
    let mut captured = capture()?;
    let state = AgentState::new(&scope().agent_id);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let phase = ResourceReadMaterial::new(&ledger, &scope())?;
    let holder = phase.guard()?;
    let queue = {
        let _entry = phase.enter();
        let mut foreign = AgentState::new("foreign");
        assert_eq!(
            captured
                .work_queue(&foreign, vec![])
                .unwrap_err()
                .to_string(),
            "Work projection State scope mismatch"
        );
        captured.live_complete = false;
        assert!(matches!(
            captured
                .work_queue(&state, vec![])
                .unwrap_err()
                .downcast_ref::<WorkReadError>(),
            Some(WorkReadError::IncompleteLive)
        ));
        captured.live_complete = true;
        captured.nonopen.limit = 0;
        captured.nonopen.is_exhaustive = false;
        assert!(matches!(
            captured
                .work_queue(&state, vec![])
                .unwrap_err()
                .downcast_ref::<WorkReadError>(),
            Some(WorkReadError::WindowExpansion { .. })
        ));
        assert_eq!(phase.finish()?, header());
        captured.nonopen.limit = WORK_NONOPEN_WINDOW;
        captured.nonopen.keys = ["done", "missing", "open-中🦀", "done", ""]
            .map(|id| WorkKey::new(WorkFamily::Work, id))
            .to_vec();
        foreign.id = state.id.clone();
        let queue = captured.work_queue(&foreign, vec![])?;
        assert_eq!(
            queue
                .items
                .iter()
                .map(|item| item.work_item.id.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["", "done", "open-中🦀"])
        );
        assert!(Arc::ptr_eq(
            &holder,
            &ResourceReadMaterial::active_guard()?.unwrap()
        ));
        queue
    };
    let total = phase.finish()?;
    drop((phase, captured));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    let json = serde_json::to_vec(&queue.items)?;
    assert_eq!(
        serde_json::from_slice::<Vec<serde_json::Value>>(&json)?.len(),
        3
    );
    struct Fail;
    impl std::io::Write for Fail {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::other("injected consumer failure"))
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    assert!(serde_json::to_writer(Fail, &queue.items).is_err());
    drop((queue, json));
    assert_eq!(ledger.report()?.observed_bytes, total.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
