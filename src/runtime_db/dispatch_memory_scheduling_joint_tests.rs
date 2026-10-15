use super::*;
#[test]
fn original_memory_scheduling_decoder_borrows_exact_input_guards_before_state_sql() -> Result<()> {
    use crate::runtime::resident_scheduling::SchedulingCaptureRequest;
    use crate::runtime_db::{
        input_evidence_snapshot::{read_material::InputReadMaterial, InputEvidenceLoadRequest},
        scheduling_snapshot::{
            prepare_scheduling_domain_snapshot_tx, SchedulingDomainLoadRequest,
            SchedulingSnapshotError,
        },
    };
    let f = Fixture::new(true)?;
    let ledger =
        crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(Default::default())?;
    let work = WorkDomainLoadRequest {
        capture: WorkCaptureRequest::current(),
        byte_limit: 2 * 1024 * 1024,
        record_limit: 65536,
    };
    let input_request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    };
    for positive in [true, false] {
        let warm = SchedulingDomainLoadRequest {
            capture: SchedulingCaptureRequest::current(),
            byte_limit: work.byte_limit,
            record_limit: work.record_limit,
        };
        drop(f.db.transaction(|tx| {
            prepare_scheduling_domain_snapshot_tx(tx, &f.scope, &warm, &ledger)
        })?);
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let result=f.db.transaction(|tx| {
            let intent=DispatchMemoryIntent::Started {start:Box::new(f.start.clone())};
            let mut memory=DispatchMemoryTransactionBudget::prepare_tx(tx,&intent,&work,&ledger)?;
            let remaining=memory.remaining_material_limits()?;let keep=if positive{1024*1024}else{6144};
            let allocation=remaining.0.checked_sub(keep+256).context("Memory original allowance too small")?;
            memory.additional_material(allocation,1,1)?;let held=vec![0u8;allocation];let before=memory.material_usage_for_test();
            let mut input=InputReadMaterial::new_with_memory(&f.scope,&input_request,&ledger,(0,0),Some(&mut memory))?;
            let mut request=SchedulingDomainLoadRequest {capture:SchedulingCaptureRequest::current(),byte_limit:input_request.byte_limit,record_limit:input_request.record_limit};
            if !positive {tx.execute_batch("ALTER TABLE agent_states RENAME TO memory_scheduling_state_forbidden")?;}
            let current=input.prepare_original_scheduling_tx(tx,&f.scope,&mut request,&ledger,false)?;
            assert!(current._read_material.is_empty());let usage=input.usage();drop((current,input));
            assert_eq!(memory.material_usage_for_test(),(before.0+usage.0,before.1+usage.1),"same Scheduling guard must debit Memory once without a second guard/metadata charge");
            assert_eq!(held.len(),allocation);drop((held,memory));Ok(())
        });
        if positive {
            result?;
        } else {
            let error = result
                .err()
                .context("Memory prior did not constrain the first Scheduling decoder")?;
            ensure!(
                matches!(
                    error.downcast_ref::<SchedulingSnapshotError>(),
                    Some(SchedulingSnapshotError::ResourceExhausted(_))
                ),
                "{error:#}"
            );
            assert!(
                f.db.agent_states().latest(&f.scope.agent_id)?.is_some(),
                "whole failed phase must restore actual schema and State"
            );
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
