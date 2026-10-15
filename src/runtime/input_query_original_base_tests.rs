use super::*;
use crate::runtime::{tests::support::LifecycleHarness, RuntimeHandle};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

async fn prepare_named(runtime: &RuntimeHandle) -> Result<()> {
    let mut held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Input Root absent")?;
    let scope = InputEvidenceScope {
        agent_id: held.agent_id().into(),
        identity_incarnation: held.identity_incarnation(),
    };
    let roles = held.input_query_roles(&scope, &runtime.inner.input_evidence_budget, LIMITS)?;
    let inputs = crate::runtime_db::input_evidence_snapshot::prepare_named_run_input_for_fixture(
        runtime.runtime_db(),
        &scope,
        &runtime.inner.input_evidence_budget,
        roles.as_ref(),
        held.owner_epoch(),
    )?;
    held.begin("actual-input-query-base-fixture".into())?;
    held.committed_prepared_with_current_read_domains(
        root.state.clone(),
        root.episodes.clone(),
        Some(Arc::new(inputs)),
        root.work.clone(),
        root.tasks.clone(),
        root.scheduling.clone(),
        root.current_reads.clone(),
    )?;
    held.publish(root.state.clone())?;
    Ok(())
}

#[tokio::test]
async fn original_input_source_copies_only_actual_named_query_base_and_keeps_its_guard_until_last_holder(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // Bootstrap now prepares its actual zero/accepted native cursor. This
    // actual evidence ACK still lacks the shared incremental installer, so its
    // real D3 invalidation is the unprepared Source exercised below.
    let entry = crate::types::TranscriptEntry::new(
        runtime.instance_agent_id(),
        crate::types::TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        serde_json::json!({"text":"actual unsupported ACK invalidates the baseline"}),
    );
    runtime.persist_transcript_evidence(&entry).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let scope = InputEvidenceScope {
        agent_id: held.agent_id().into(),
        identity_incarnation: held.identity_incarnation(),
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let fresh = held.input_query_source(&scope, ledger, LIMITS)?;
    assert!(
        fresh.roles().original_base().is_none(),
        "unprepared Source cannot synthesize a zero complete baseline"
    );
    drop((fresh, held));
    prepare_named(runtime).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual named Root absent")?;
    let input = root
        .input_evidence
        .as_ref()
        .context("actual named Input absent")?;
    assert!(input.query_coverage().birth_cursor.is_some());
    let before = ledger.report()?.observed_bytes;
    let source = held.input_query_source(&scope, ledger, LIMITS)?;
    assert_eq!(source.roles().original_base(), Some(input.query_coverage()));
    let mut observed = (0u64, 0usize);
    source.roles().visit_material(|guard, bytes, nodes| {
        guard.validate_owned_material_binding(&scope, ledger, bytes)?;
        observed.0 = observed
            .0
            .checked_add(bytes)
            .context("actual base bytes overflow")?;
        observed.1 = observed
            .1
            .checked_add(nodes)
            .context("actual base nodes overflow")?;
        Ok(())
    })?;
    assert_eq!(observed, source.roles().material_estimate());
    let last = source.clone();
    drop(source);
    assert!(ledger.report()?.observed_bytes > before);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, before);
    assert_eq!(
        root.input_evidence.as_ref().unwrap().query_coverage(),
        input.query_coverage()
    );
    Ok(())
}

#[tokio::test]
async fn original_input_source_base_is_admitted_before_clones_and_foreign_accounting_cannot_borrow_it(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    prepare_named(runtime).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let scope = InputEvidenceScope {
        agent_id: held.agent_id().into(),
        identity_incarnation: held.identity_incarnation(),
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let source = held.input_query_source(&scope, ledger, LIMITS)?;
    let exact = source.roles().material_estimate();
    assert!(source.roles().original_base().is_some());
    drop(source);
    let before = ledger.report()?.observed_bytes;
    let error = held
        .input_query_source(&scope, ledger, (usize::try_from(exact.0)? - 1, LIMITS.1))
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::ResourceExhausted("original query base"))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    let other = InputEvidenceBudgetLedger::new(Default::default())?;
    let error = held.input_query_source(&scope, &other, LIMITS).unwrap_err();
    assert!(format!("{error:#}").contains("scope/ledger"));
    assert_eq!(ledger.report()?.observed_bytes, before);
    assert_eq!(other.report()?.observed_bytes, 0);
    Ok(())
}

#[tokio::test]
async fn original_input_source_checks_low_actual_ledger_before_snapshot_or_named_base_clone(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    prepare_named(runtime).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual named Root absent")?;
    let scope = InputEvidenceScope {
        agent_id: held.agent_id().into(),
        identity_incarnation: held.identity_incarnation(),
    };
    let coverage = root
        .input_evidence
        .as_ref()
        .context("actual named Input absent")?
        .query_coverage();
    let registry = InputQueryRegistry::default();
    let roomy = InputEvidenceBudgetLedger::new(Default::default())?;
    let measured = registry
        .snapshot(&scope, held.owner_epoch(), &roomy, LIMITS)?
        .with_original_base(coverage, &roomy, LIMITS)?;
    let cost = measured.material_estimate();
    drop(measured);
    assert_eq!(roomy.report()?.observed_bytes, 0);
    for native in [false, true] {
        let mut policy = crate::config::StateMemoryLimits::default();
        if native {
            policy.record_limit = u64::try_from(cost.1 - 1)?;
        } else {
            policy.history_per_agent_bytes = cost.0 - 2;
            policy.pending_per_agent_bytes = 1;
        }
        let low = InputEvidenceBudgetLedger::new(policy)?;
        // Borrow only actual named coverage; no Ready DTO is manufactured.
        let snapshot = registry.snapshot(&scope, held.owner_epoch(), &low, LIMITS)?;
        let before = low.report()?.observed_bytes;
        let error = snapshot
            .with_original_base(coverage, &low, LIMITS)
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted("original query base"))
            ),
            "{error:#}"
        );
        assert!(before > 0);
        assert_eq!(low.report()?.observed_bytes, 0);
    }
    let mut policy = crate::config::StateMemoryLimits::default();
    policy.record_limit = 1;
    let low = InputEvidenceBudgetLedger::new(policy)?;
    let error = held.input_query_source(&scope, &low, LIMITS).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(low.report()?.observed_bytes, 0);
    Ok(())
}
