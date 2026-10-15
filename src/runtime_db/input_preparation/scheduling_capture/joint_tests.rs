use super::*;
use crate::runtime::RuntimeHandle;
use crate::runtime_db::input_evidence_snapshot::{
    read_material::InputReadMaterial, InputEvidenceLoadRequest,
};
fn policy(bytes: usize, records: usize) -> InputEvidenceLoadRequest {
    InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        byte_limit: bytes,
        record_limit: records,
    }
}
#[tokio::test]
async fn actual_scheduling_selector_and_sql_share_held_input_phase_and_last_guard() -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test("joint-scheduling-input",|db,scope,state,ledger| {
        let full=policy(64*1024*1024,65536);let baseline=ledger.report()?.observed_bytes;
        let capture=SchedulingCaptureRequest::current();let allocation=1024*1024;
        let mut alone=InputReadMaterial::new(scope,&full,ledger,(0,0))?;
        alone.observe_projected(u64::try_from(allocation)?,1)?;let held=vec![0u8;allocation];
        let prior=alone.usage();
        let mut request=PreparedSchedulingRequest::using_input_material(scope,Some(&capture),ledger,&mut alone)?;
        assert!(request._selector.is_none(),"the original Input owns the sole selector guard");
        let current=db.transaction(|tx|alone.prepare_original_scheduling_tx(tx,scope,&mut request.request,ledger,false))?;
        assert_eq!(current.facts.state.as_ref(),Some(state));
        let added=(alone.usage().0-prior.0,alone.usage().1-prior.1);assert!(added.0>0 && added.1>0);
        drop((current,request,alone,held));assert_eq!(ledger.report()?.observed_bytes,baseline);
        for by_records in [false,true] {
            let limited=policy(if by_records {full.byte_limit}else{prior.0+added.0-1},if by_records{prior.1+added.1-1}else{full.record_limit});
            let before:(u64,String)=db.connection()?.query_row("SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",rusqlite::params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let mut input=InputReadMaterial::new(scope,&limited,ledger,(0,0))?;
            input.observe_projected(u64::try_from(allocation)?,1)?;let held=vec![0u8;allocation];
            let mut request=PreparedSchedulingRequest::using_input_material(scope,Some(&capture),ledger,&mut input)?;
            let error=db.transaction(|tx|input.prepare_original_scheduling_tx(tx,scope,&mut request.request,ledger,false)).err().context("Scheduling received a second full allowance")?;
            ensure!(matches!(error.downcast_ref::<SchedulingSnapshotError>(),Some(SchedulingSnapshotError::ResourceExhausted(_))),"{error:#}");
            assert_eq!(db.connection()?.query_row("SELECT revision,coverage_json FROM scheduling_domain_current WHERE agent_id=?1 AND identity_incarnation=?2",rusqlite::params![scope.agent_id,scope.identity_incarnation],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,String>(1)?)))?,before);
            assert_eq!(held.len(),allocation);drop((request,input,held));assert_eq!(ledger.report()?.observed_bytes,baseline);
        }
        let mut input=InputReadMaterial::new(scope,&full,ledger,(0,0))?;
        input.observe_projected(u64::try_from(allocation)?,1)?;let held=vec![0u8;allocation];
        let mut request=PreparedSchedulingRequest::using_input_material(scope,Some(&capture),ledger,&mut input)?;
        let current=db.transaction(|tx|input.prepare_original_scheduling_tx(tx,scope,&mut request.request,ledger,false))?;
        assert!(current._read_material.is_empty(),"actual decoder guards must transfer to the same Input phase");
        let guards=input.into_guards();let last=guards.last().context("actual Scheduling guard was not transferred")?.clone();let weak=Arc::downgrade(&last);
        drop((current,request,guards,held));assert!(weak.upgrade().is_some());drop(last);assert!(weak.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes,baseline);Ok(())
    }).await
}
#[tokio::test]
async fn scheduling_selector_uses_input_remaining_before_clone_and_foreign_phase_before_sql(
) -> Result<()> {
    RuntimeHandle::with_original_input_current_fixture_for_test(
        "joint-scheduling-selector",
        |db, scope, _state, ledger| {
            let baseline = ledger.report()?.observed_bytes;
            let full = policy(64 * 1024 * 1024, 65536);
            let capture = SchedulingCaptureRequest {
                include_live: false,
                history_limit: 0,
                required_keys: (0..128)
                    .map(|i| {
                        SchedulingKey::new(
                            crate::runtime::resident_scheduling::SchedulingFamily::Timer,
                            format!("{i}-{}", "x".repeat(64)),
                        )
                    })
                    .collect(),
            };
            let cost = selector_material(&capture)?;
            let mut measured = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
            let allocation = 1024 * 1024;
            measured.observe_projected(u64::try_from(allocation)?, 1)?;
            let prior = measured.usage();
            drop(measured);
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            db.connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO scheduling_selector_state_forbidden",
            )?;
            for by_records in [false, true] {
                let limited = policy(
                    if by_records {
                        full.byte_limit
                    } else {
                        prior.0 + cost.0 - 1
                    },
                    if by_records {
                        prior.1 + cost.1 - 1
                    } else {
                        full.record_limit
                    },
                );
                let mut input = InputReadMaterial::new(scope, &limited, ledger, (0, 0))?;
                input.observe_projected(u64::try_from(allocation)?, 1)?;
                let held = vec![0u8; allocation];
                let before = input.usage();
                let error = PreparedSchedulingRequest::using_input_material(
                    scope,
                    Some(&capture),
                    ledger,
                    &mut input,
                )
                .err()
                .context("over-budget required set was cloned")?;
                ensure!(
                    matches!(
                        error.downcast_ref::<SchedulingSnapshotError>(),
                        Some(SchedulingSnapshotError::ResourceExhausted(
                            "Input-owned Scheduling selector"
                        ))
                    ),
                    "{error:#}"
                );
                assert_eq!(input.usage(), before);
                assert_eq!(capture.required_keys.len(), 128);
                drop((input, held));
                assert_eq!(ledger.report()?.observed_bytes, baseline);
            }
            let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
            let mut input = InputReadMaterial::new(scope, &full, ledger, (0, 0))?;
            let before = input.usage();
            let error = PreparedSchedulingRequest::using_input_material(
                scope,
                Some(&capture),
                &foreign,
                &mut input,
            )
            .err()
            .context("foreign selector ledger accepted")?;
            assert!(
                error.to_string().contains("another original ledger"),
                "{error:#}"
            );
            let mut request = SchedulingDomainLoadRequest {
                capture: SchedulingCaptureRequest::current(),
                record_limit: full.record_limit,
                byte_limit: full.byte_limit,
            };
            let error = db
                .transaction(|tx| {
                    input.prepare_original_scheduling_tx(tx, scope, &mut request, &foreign, false)
                })
                .err()
                .context("foreign Scheduling phase reached SQL")?;
            assert!(
                error.to_string().contains("another original ledger"),
                "{error:#}"
            );
            assert_eq!(input.usage(), before);
            assert_eq!(foreign.report()?.observed_bytes, 0);
            drop(input);
            assert_eq!(ledger.report()?.observed_bytes, baseline);
            db.connection()?.execute_batch(
                "ALTER TABLE scheduling_selector_state_forbidden RENAME TO agent_states",
            )?;
            Ok(())
        },
    )
    .await
}
