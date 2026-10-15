use super::*;
use crate::runtime::tests::support::LifecycleHarness;
const LIMITS:(usize,usize)=(64*1024*1024,65536);

#[tokio::test]
async fn actual_runtime_acquisition_guard_is_lent_once_before_source_selector_copy_and_last_source_holder() -> Result<()> {
    let h=LifecycleHarness::new();let runtime=h.runtime();
    runtime.quiesce_context_resource_observer_task_for_test().await?;
    let ledger=&runtime.inner.input_evidence_budget;
    let baseline=ledger.report()?.observed_bytes;
    let mutation=runtime.begin_mutation().await?;
    let source=mutation.original_acquisition_input_source_for_test(LIMITS)?;
    let material=source.roles().original_acquisition.as_ref().context("actual acquisition missing from Source")?;
    let weak=Arc::downgrade(material);
    let cost=material.material_estimate();
    assert!(cost.0>0 && cost.1>0);
    let mut actual=(0u64,0usize);let mut visits=0;
    material.visit_material(&mut |guard,bytes,nodes| {
        source.roles().visit_material(|selected,selected_bytes,selected_nodes| {
            if Arc::ptr_eq(guard,selected) {
                visits+=1;assert_eq!((selected_bytes,selected_nodes),(bytes,nodes));
            }
            actual.0=actual.0.checked_add(selected_bytes).context("Source byte overflow")?;
            actual.1=actual.1.checked_add(selected_nodes).context("Source nodes overflow")?;
            Ok(())
        })
    })?;
    assert_eq!(visits,1,"the existing acquisition allocation was duplicated");
    assert_eq!(actual,source.roles().material_estimate());
    let before=ledger.report()?.observed_bytes;
    let error=mutation.original_acquisition_input_source_for_test((usize::try_from(cost.0)?-1,LIMITS.1))
        .err().context("Source copied selectors before paying its real acquisition prior")?;
    assert!(error.downcast_ref::<InputQueryError>().is_some_and(|e|matches!(e,InputQueryError::ResourceExhausted(_))),"{error:#}");
    assert_eq!(ledger.report()?.observed_bytes,before,"rejected first copy allocated a new registry guard");
    let last=source.clone();drop(source);drop(mutation);
    assert!(weak.upgrade().is_some(),"Source lost the actual candidate/base allocation before its last holder");
    drop(last);assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes,baseline);
    Ok(())
}

#[tokio::test]
async fn actual_acquisition_source_rejects_another_scope_epoch_or_ledger_without_registering_it_again() -> Result<()> {
    let h=LifecycleHarness::new();let runtime=h.runtime();
    runtime.quiesce_context_resource_observer_task_for_test().await?;
    let mutation=runtime.begin_mutation().await?;
    let source=mutation.original_acquisition_input_source_for_test(LIMITS)?;
    let material=source.roles().original_acquisition.as_ref().context("actual acquisition missing")?;
    let ledger=&runtime.inner.input_evidence_budget;
    let scope=&source.roles().scope;let epoch=source.owner_epoch();
    let baseline=ledger.report()?.observed_bytes;
    let other=InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(material.validate(scope,epoch,&other).is_err());
    assert!(material.validate(scope,epoch.checked_add(1).context("epoch overflow")?,ledger).is_err());
    let foreign=InputEvidenceScope{agent_id:format!("{}-foreign",scope.agent_id),identity_incarnation:scope.identity_incarnation};
    assert!(material.validate(&foreign,epoch,ledger).is_err());
    assert!(source.roles().validate_binding(scope,epoch,&other).is_err());
    assert_eq!(other.report()?.observed_bytes,0);
    assert_eq!(ledger.report()?.observed_bytes,baseline);
    Ok(())
}
