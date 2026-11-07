use super::*;
use crate::{runtime::tests::support::LifecycleHarness, types::*};

#[tokio::test]
async fn birth_membership_does_not_grant_body_authority_or_replace_the_source_ledger() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let message = runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "native body".into(),
            },
        ))
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let inputs = root.input_evidence.as_ref().unwrap();
    let selection = InputQuerySelection {
        birth_after: inputs.query_coverage()?.birth_after,
        ..Default::default()
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let foreign_before = foreign.report()?.observed_bytes;
    assert!(inputs.capture_birth_inputs(&selection, &foreign).is_err());
    assert_eq!(foreign.report()?.observed_bytes, foreign_before);
    let key = InputEvidenceKey::new(InputEvidenceFamily::Message, &message.id);
    let mut missing = inputs.as_ref().clone();
    missing.records.remove(&key);
    let error = missing
        .capture_birth_inputs(&selection, ledger)
        .unwrap_err();
    assert_eq!(
        error.downcast_ref::<InputEvidenceReadError>(),
        Some(&InputEvidenceReadError::NeedsPreparation(key))
    );
    let source = inputs.query_acquisition.as_ref().unwrap();
    let error = source
        .read(|| {
            crate::diagnostics::resource_io::admit_owned_metadata(u64::MAX, 1)?;
            Ok(())
        })
        .unwrap_err();
    let before = ledger.report()?.observed_bytes;
    let retry = inputs.capture_birth_inputs(&selection, ledger).unwrap_err();
    assert!(error.to_string().contains("ResourceExhausted"));
    let expected = crate::runtime_error::describe_runtime_error(&source.finish().unwrap_err());
    let actual = crate::runtime_error::describe_runtime_error(&retry);
    assert_eq!(actual.code, expected.code, "{retry:#}");
    assert_eq!(actual.domain, expected.domain);
    assert!(actual.retryable);
    assert_eq!(ledger.report()?.observed_bytes, before);
    Ok(())
}

#[tokio::test]
async fn new_owner_source_preserves_previous_cut_allowance_and_failure_latch() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let (_, _session) = runtime.begin_task_birth_session().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let inputs = root.input_evidence.as_ref().unwrap();
    let original = inputs.query_acquisition.as_ref().unwrap();
    let original_usage = original.finish()?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    for _ in 0..8 {
        let source = inputs.original_query_source()?.unwrap();
        assert_eq!(original.finish()?, original_usage);
        assert!(source.acquisition().finish()?.0 > 0);
        drop(source);
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    original
        .read(|| {
            crate::diagnostics::resource_io::admit_owned_metadata(u64::MAX, 1)?;
            Ok(())
        })
        .unwrap_err();
    let baseline = ledger.report()?.observed_bytes;
    let error = inputs.original_query_source().unwrap_err();
    assert_eq!(
        crate::runtime_error::describe_runtime_error(&error).code,
        "context_resource_exhausted"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
