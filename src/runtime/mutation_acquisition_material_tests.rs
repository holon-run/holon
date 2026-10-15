use super::*;
use crate::runtime::tests::support::LifecycleHarness;

#[tokio::test]
async fn actual_mutation_candidate_material_precedes_state_and_shared_queue_copy_and_drops_after_its_owner(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("actual Root missing")?;
    let strong = Arc::strong_count(&root);
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE tasks RENAME TO acquisition_forbidden_tasks; ALTER TABLE transcript_entries RENAME TO acquisition_forbidden_transcript;")?;
    let owner = runtime.begin_mutation().await;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE acquisition_forbidden_tasks RENAME TO tasks; ALTER TABLE acquisition_forbidden_transcript RENAME TO transcript_entries;")?;
    let owner = owner?;
    // This is the real original Source, not a separately minted proof object.
    let source = owner.original_acquisition_input_source_for_test((64 * 1024 * 1024, 65536))?;
    let mut acquisition_seen = 0usize;
    source.roles().visit_material(|guard, bytes, nodes| {
        if bytes > 0 && nodes > 0 {
            guard.validate_owned_material_binding(
                &resident_input_evidence::InputEvidenceScope {
                    agent_id: runtime.instance_agent_id().into(),
                    identity_incarnation: runtime.instance_incarnation(),
                },
                ledger,
                bytes,
            )?;
            acquisition_seen += 1;
        }
        Ok(())
    })?;
    assert!(acquisition_seen > 0);
    assert_eq!(Arc::strong_count(&root), strong);
    drop(source);
    drop(owner);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let agent = runtime.inner.agent.lock().await;
    let measured = OriginalMutationAcquisitionMaterial::for_agent(
        runtime,
        &agent,
        0,
        1,
        (0, 0),
        (64 * 1024 * 1024, 65536),
    )?;
    let actual = measured.material_estimate();
    drop(measured);
    let before = ledger.report()?.observed_bytes;
    for limits in [
        (usize::try_from(actual.0)? - 1, 65536),
        (64 * 1024 * 1024, actual.1 - 1),
    ] {
        let error =
            OriginalMutationAcquisitionMaterial::for_agent(runtime, &agent, 0, 1, (0, 0), limits)
                .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, before);
    }
    Ok(())
}
