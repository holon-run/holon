use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::types::TranscriptEntryKind;
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

#[tokio::test]
async fn original_input_publication_prior_is_bounded_before_its_scope_copy_and_keeps_only_its_actual_guard(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let before = ledger.report()?.observed_bytes;
    let material = inputs.original_publication_material(ledger, LIMITS, (0, 0))?;
    let cost = material.material_estimate();
    assert!(cost.0 > 0 && cost.1 > 0);
    let planned = material.publication_copy_estimate();
    assert!(planned.0 > 0 && planned.1 > 0);
    assert!(planned.0 <= LIMITS.0 as u64 && planned.1 <= LIMITS.1);
    assert!(material.matches_original(inputs, ledger)?);
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, before);
    for limits in [
        (usize::try_from(cost.0)? - 1, LIMITS.1),
        (LIMITS.0, cost.1 - 1),
    ] {
        let error = inputs
            .original_publication_material(ledger, limits, (0, 0))
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted("publication metadata"))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, before);
    }
    // The future publication estimate may exceed the descriptor. These cases
    // isolate the real, currently live descriptor against an existing prior.
    for prior in [
        (LIMITS.0 as u64 - cost.0 + 1, 0),
        (0, LIMITS.1 - cost.1 + 1),
    ] {
        let error = inputs
            .original_publication_material(ledger, LIMITS, prior)
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<InputQueryError>(),
                Some(InputQueryError::ResourceExhausted("publication metadata"))
            ),
            "{error:#}"
        );
        assert_eq!(ledger.report()?.observed_bytes, before);
    }
    let material = inputs.original_publication_material(ledger, LIMITS, (0, 0))?;
    let other = InputEvidenceBudgetLedger::new(Default::default())?;
    assert!(material.validate_binding(inputs.scope(), &other).is_err());
    assert_eq!(other.report()?.observed_bytes, 0);
    drop(material);
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}
#[tokio::test]
async fn original_input_source_publication_prior_uses_the_published_metadata_even_without_canonical_body_tables_and_does_not_mint_ready(
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
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let source = held.input_query_source(inputs.scope(), ledger, LIMITS)?;
    assert!(source.roles().original_base().is_none());
    assert!(source
        .roles()
        .original_publication_matches(inputs, ledger)?);
    drop(source);
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("DROP TABLE messages")?;
    let before = ledger.report()?.observed_bytes;
    let source = held.input_query_source(inputs.scope(), ledger, LIMITS)?;
    assert!(source.roles().original_base().is_none());
    assert!(source
        .roles()
        .original_publication_matches(inputs, ledger)?);
    let mut actual = (0u64, 0usize);
    source.roles().visit_material(|guard, bytes, nodes| {
        guard.validate_owned_material_binding(inputs.scope(), ledger, bytes)?;
        actual.0 = actual
            .0
            .checked_add(bytes)
            .context("actual prior bytes overflow")?;
        actual.1 = actual
            .1
            .checked_add(nodes)
            .context("actual prior nodes overflow")?;
        Ok(())
    })?;
    assert_eq!(actual, source.roles().material_estimate());
    let last = source.clone();
    drop(source);
    assert!(ledger.report()?.observed_bytes > before);
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}
#[tokio::test]
async fn original_input_publication_seal_rejects_an_actual_later_transcript_ack_without_pinning_its_old_root_or_rehydrating_body(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    // Use the actual owner preparation actor for the original ordinary window.
    {
        let mut owner = runtime.begin_mutation().await?;
        let (_, records, bytes) = owner.context_input_evidence_policy()?;
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            windows: vec![
                crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                    selector: InputWindowSelector::RecentTranscript,
                    limit: 1,
                },
            ],
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: records,
            byte_limit: bytes,
        };
        owner.prepare_owner_input_request(&request, 8).await?;
    }
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let original = inputs.original_publication_material(ledger, LIMITS, (0, 0))?;
    let old_input = Arc::downgrade(inputs);
    let scope = inputs.scope().clone();
    drop((root, held));
    let entry = TranscriptEntry::new(
        &scope.agent_id,
        TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        serde_json::json!({"text":"actual publication source changed"}),
    );
    runtime.persist_transcript_evidence(&entry).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual later Root missing")?;
    let after = root
        .input_evidence
        .as_ref()
        .context("actual later Input missing")?;
    assert!(
        after.records.contains_key(&InputEvidenceKey::new(
            InputEvidenceFamily::Transcript,
            &entry.id
        )),
        "actual ordinary window did not retain the accepted Transcript"
    );
    assert!(!original.matches_original(after, ledger)?);
    assert!(
        old_input.upgrade().is_none(),
        "metadata loan retained the old Input/Root"
    );
    original.validate_binding(&scope, ledger)?;
    Ok(())
}

#[tokio::test]
async fn original_source_accepts_only_its_current_descriptor_when_a_future_copy_exceeds_the_caller_and_copy_still_checks_its_real_remaining(
) -> Result<()> {
    let h = LifecycleHarness::new();
    let runtime = h.runtime();
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    {
        let mut owner = runtime.begin_mutation().await?;
        let (_, records, bytes) = owner.context_input_evidence_policy()?;
        let request = crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
            windows: vec![
                crate::runtime_db::input_evidence_snapshot::InputWindowRequest {
                    selector: InputWindowSelector::RecentTranscript,
                    limit: 1,
                },
            ],
            mandatory_roots: Default::default(),
            interrupted_sources: Default::default(),
            selected_completion_brief_ids: Default::default(),
            record_limit: records,
            byte_limit: bytes,
        };
        owner.prepare_owner_input_request(&request, 8).await?;
    }
    let entry = TranscriptEntry::new(
        runtime.instance_agent_id(),
        TranscriptEntryKind::AssistantRound,
        Some(0),
        None,
        serde_json::json!({"text":"actual finite source whose copied coverage has a separate lifetime"}),
    );
    runtime.persist_transcript_evidence(&entry).await?;
    let held = runtime.inner.mutation_owner.reserve().await?;
    let root = held
        .capture_ready_snapshot()?
        .context("actual Root missing")?;
    let inputs = root
        .input_evidence
        .as_ref()
        .context("actual Input missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let source = held.input_query_source(inputs.scope(), ledger, LIMITS)?;
    let actual = source.roles().material_estimate();
    drop(source);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let descriptor = inputs.original_publication_material(ledger, LIMITS, (0, 0))?;
    let future = descriptor.publication_copy_estimate();
    assert!(
        future.0 > actual.0 || future.1 > actual.1,
        "fixture requires a future copy larger than current Source: {future:?} / {actual:?}"
    );
    let limits = if future.0 > actual.0 {
        (usize::try_from(actual.0)?, LIMITS.1)
    } else {
        (LIMITS.0, actual.1)
    };
    let source = held.input_query_source(inputs.scope(), ledger, limits)?;
    assert_eq!(source.roles().material_estimate(), actual);
    assert!(source
        .roles()
        .original_publication_matches(inputs, ledger)?);
    let own = descriptor.material_estimate();
    let prior = (
        actual
            .0
            .checked_add(own.0)
            .context("actual prior bytes overflow")?,
        actual
            .1
            .checked_add(own.1)
            .context("actual prior nodes overflow")?,
    );
    let before = ledger.report()?.observed_bytes;
    let too_small = (
        usize::try_from(
            prior
                .0
                .checked_add(future.0)
                .context("copy bytes overflow")?,
        )? - 1,
        LIMITS.1,
    );
    let error = descriptor
        .copy_for_test(inputs, ledger, too_small, prior)
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputQueryError>(),
            Some(InputQueryError::ResourceExhausted("publication metadata"))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, before);
    let (copy, copy_guard) = descriptor.copy_for_test(inputs, ledger, LIMITS, prior)?;
    assert_eq!(copy.query_coverage(), inputs.query_coverage());
    assert!(copy
        .records
        .values()
        .zip(inputs.records.values())
        .all(|(left, right)| Arc::ptr_eq(left, right)));
    assert!(ledger.report()?.observed_bytes > before);
    drop((copy, copy_guard));
    assert_eq!(ledger.report()?.observed_bytes, before);
    drop((descriptor, source));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
