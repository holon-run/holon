use super::*;
use crate::diagnostics::resource_io::ResourceReadMaterial;

#[test]
fn selected_domain_moves_metadata_and_preserves_capture_facts() -> Result<()> {
    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let original = resident(true).with_budget(ledger.clone())?;
    let expected = original.capture()?;
    let scope_address = original.scope.agent_id.as_ptr();
    let windows_address = original.coverage.windows.as_ptr();
    let turn_address = original
        .coverage
        .turn_transcript_keys
        .keys()
        .next()
        .unwrap()
        .as_ptr();
    let record = Arc::downgrade(&expected.records[0]);
    let captured = original.into_capture()?;
    assert_eq!(captured.scope.agent_id.as_ptr(), scope_address);
    assert_eq!(captured.windows.as_ptr(), windows_address);
    assert_eq!(
        captured
            .turn_transcript_keys
            .keys()
            .next()
            .unwrap()
            .as_ptr(),
        turn_address
    );
    assert_eq!(captured.scope, expected.scope);
    assert_eq!(captured.windows, expected.windows);
    assert_eq!(captured.execution, expected.execution);
    assert_eq!(
        captured.recovery_source_absences,
        expected.recovery_source_absences
    );
    assert_eq!(captured.unavailable, expected.unavailable);
    assert_eq!(captured.turn_transcript_keys, expected.turn_transcript_keys);
    assert_eq!(captured.message_turn_keys, expected.message_turn_keys);
    assert_eq!(captured.records, expected.records);
    assert!(Arc::ptr_eq(&captured.records[0], &expected.records[0]));
    assert_eq!(captured.protocol_state()?, expected.protocol_state()?);
    drop(expected);
    assert!(record.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > 0);
    drop(captured);
    assert!(record.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}

#[test]
fn transfer_admits_slots_and_registration_and_rejects_each_short_axis() -> Result<()> {
    let count = resident(true).records.len();
    // Both the phase header and the retained owned-metadata registration keep
    // their original bookkeeping admission, in addition to the new slots.
    let registration_bytes = (scope().agent_id.len() as u64 + 256) * 8;
    let bytes = registration_bytes * 2
        + ((count * std::mem::size_of::<Arc<QualifiedInputEvidence>>()
            + std::mem::size_of::<Vec<Arc<QualifiedInputEvidence>>>()
            + 2 * std::mem::size_of::<usize>()) as u64
            * 8);
    let records = 64 * 2 + (count + 1) * 8;
    for (byte_limit, record_limit) in [(bytes, records), (bytes - 1, records), (bytes, records - 1)]
    {
        let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
        let original = resident(true).with_budget(ledger.clone())?;
        let phase =
            ResourceReadMaterial::with_test_limits(&ledger, &scope(), byte_limit, record_limit)?;
        let result = phase.read(|| original.into_capture());
        if (byte_limit, record_limit) == (bytes, records) {
            let captured = result?;
            assert_eq!(phase.finish()?, (bytes, records));
            assert_eq!(captured.records.len(), count);
            assert_eq!(captured.records.capacity(), count);
            drop(captured);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(phase.read(|| Ok(())).is_err());
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn request_transfer_keeps_original_qualification_and_failure_latch() -> Result<()> {
    use crate::runtime_db::input_evidence_snapshot::{
        InputEvidenceLoadRequest, InputWindowRequest,
    };

    let ledger = InputEvidenceBudgetLedger::new(crate::config::StateMemoryLimits::default())?;
    let original = resident(true).with_budget(ledger.clone())?;
    let request = InputEvidenceLoadRequest {
        query: None,
        windows: vec![InputWindowRequest {
            selector: InputWindowSelector::GlobalTurns,
            limit: 1,
        }],
        mandatory_roots: BTreeSet::new(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 8,
        byte_limit: 4096,
    };
    let fresh = ResourceReadMaterial::new(&ledger, &scope())?;
    let selected = original.capture_for_request_with_material(&request, &fresh)?;
    let cost = fresh.finish()?;
    assert_eq!(selected.records.len(), 1);
    assert_eq!(selected.records[0].locator.key.id, "b");
    let selected_record = Arc::clone(&selected.records[0]);
    drop(selected);
    drop(fresh);
    for (bytes, records) in [cost, (cost.0 - 1, cost.1), (cost.0, cost.1 - 1)] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope(), bytes, records)?;
        let result = original.capture_for_request_with_material(&request, &phase);
        if (bytes, records) == cost {
            let captured = result?;
            assert!(Arc::ptr_eq(&captured.records[0], &selected_record));
            assert_eq!(phase.finish()?, cost);
        } else {
            assert!(result.is_err());
            assert!(phase.finish().is_err());
            assert!(original
                .capture_for_request_with_material(&request, &phase)
                .is_err());
        }
    }
    Ok(())
}
