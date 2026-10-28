use super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::{InputEvidenceBudgetLedger, InputMemoryCategory},
        resident_input_evidence::InputEvidenceScope,
        resident_scheduling::{SchedulingCaptureRequest, SchedulingFamily, SchedulingKey},
    },
    runtime_db::scheduling_snapshot::{
        prepare_cold_scheduling_query_tx, ColdSchedulingQuery, SchedulingCurrentSnapshot,
        SchedulingDomainLoadRequest,
    },
};

fn scope(identity: &AgentIdentityRecord) -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    }
}

fn request() -> SchedulingDomainLoadRequest {
    SchedulingDomainLoadRequest {
        capture: SchedulingCaptureRequest::current(),
        byte_limit: 64 * 1024 * 1024,
        record_limit: 65536,
    }
}

async fn fixture(
    populated: bool,
) -> Result<(
    tempfile::TempDir,
    RuntimeHost,
    AgentIdentityRecord,
    SchedulingCurrentSnapshot,
)> {
    let (home, host) = test_host();
    let identity = host.ensure_default_agent_identity()?;
    let config = host.config().as_ref().clone();
    if populated {
        let runtime = host.default_runtime().await?;
        runtime
            .schedule_timer(3_600_000, None, Some("capture 汉🦀".repeat(5)))
            .await?;
        runtime
            .default_external_trigger(crate::types::CallbackDeliveryMode::WakeHint)
            .await?;
        drop(runtime);
    } else {
        let state = AgentState::new(&identity.agent_id);
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(&state, identity.incarnation)?;
    }
    host.shutdown().await?;
    drop(host);
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let read = host.runtime_db().transaction(|tx| {
        prepare_cold_scheduling_query_tx(
            tx,
            &scope(&identity),
            &request(),
            &host.inner.input_evidence_budget,
        )
    })?;
    let ColdSchedulingQuery::Current(current) = read else {
        anyhow::bail!("fixture must have a current State");
    };
    Ok((home, host, identity, *current))
}

fn candidate_bytes(host: &RuntimeHost) -> Result<u64> {
    Ok(host
        .inner
        .input_evidence_budget
        .report()?
        .categories
        .get(&InputMemoryCategory::Candidate)
        .map_or(0, |usage| usage.bytes))
}

#[tokio::test]
async fn cold_capture_phase_standalone_shares_bodies_and_keeps_last_holder() -> Result<()> {
    for populated in [false, true] {
        let (_home, host, identity, source) = fixture(populated).await?;
        let expected = source.resident.capture(&request().capture)?;
        let baseline = candidate_bytes(&host)?;
        let selected =
            host.capture_cold_scheduling(&identity, &source.resident, &request().capture)?;
        assert_eq!(selected.scope, expected.scope);
        assert_eq!(selected.revision, expected.revision);
        assert_eq!(
            serde_json::to_value(&selected.coverage)?,
            serde_json::to_value(&expected.coverage)?,
        );
        assert_eq!(selected.records, expected.records);
        assert_eq!(selected.records.is_empty(), !populated);
        for (actual, original) in selected.records.iter().zip(&expected.records) {
            assert!(Arc::ptr_eq(actual, original));
        }
        let held = candidate_bytes(&host)?;
        assert!(
            held > baseline,
            "even empty capture owns selection metadata"
        );
        let first = Arc::new(selected);
        let last = first.clone();
        drop(first);
        assert_eq!(candidate_bytes(&host)?, held);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        drop(last);
        assert_eq!(candidate_bytes(&host)?, baseline);
        drop((expected, source));
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_capture_phase_reuses_exact_allowance_and_latches_both_limits() -> Result<()> {
    for populated in [false, true] {
        let (_home, host, identity, source) = fixture(populated).await?;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let oracle = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
        let expected = {
            let _entry = oracle.enter();
            source.resident.capture(&request().capture)?
        };
        let usage = oracle.finish()?;
        drop((expected, oracle));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        for shortage in [None, Some(true), Some(false)] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(&identity),
                usage.0 - u64::from(shortage == Some(true)),
                usage.1 - usize::from(shortage == Some(false)),
            )?;
            let weak = Arc::downgrade(&phase.guard()?);
            let baseline = candidate_bytes(&host)?;
            let result = {
                let _entry = phase.enter();
                host.capture_cold_scheduling(&identity, &source.resident, &request().capture)
            };
            assert_eq!(candidate_bytes(&host)?, baseline);
            if shortage.is_none() {
                let selected = Arc::new(result?);
                assert_eq!(phase.finish()?, usage);
                let last = selected.clone();
                drop((phase, selected));
                assert!(weak.upgrade().is_some());
                drop(last);
            } else {
                let error = result.unwrap_err();
                assert!(
                    format!("{error:#}")
                        .contains("ResourceExhausted: context resource read material"),
                    "{error:#}"
                );
                let latched = phase.finish().unwrap_err();
                let failed_bytes = ledger.report()?.observed_bytes;
                let retry = {
                    let _entry = phase.enter();
                    host.capture_cold_scheduling(&identity, &source.resident, &request().capture)
                        .unwrap_err()
                };
                assert_eq!(format!("{retry:#}"), format!("{latched:#}"));
                assert_eq!(
                    retry
                        .downcast_ref::<crate::runtime_error::RuntimeError>()
                        .context("retry must preserve the original resource latch")?
                        .descriptor()
                        .code,
                    "context_resource_exhausted",
                );
                assert_eq!(ledger.report()?.observed_bytes, failed_bytes);
                drop(phase);
            }
            assert!(weak.upgrade().is_none());
            assert_eq!(ledger.report()?.observed_bytes, 0);
            assert!(ResourceReadMaterial::active_guard()?.is_none());
        }
        drop(source);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_capture_phase_preserves_lookup_error_and_releases_failed_capture() -> Result<()> {
    let (_home, host, identity, source) = fixture(true).await?;
    let mut missing = request().capture;
    missing.required_keys.insert(SchedulingKey::new(
        SchedulingFamily::Timer,
        "missing capture 汉🦀",
    ));
    let expected = source.resident.capture(&missing).unwrap_err();
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let error = host
        .capture_cold_scheduling(&identity, &source.resident, &missing)
        .unwrap_err();
    assert_eq!(format!("{error:#}"), format!("{expected:#}"));
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop(source);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_capture_phase_three_real_callers_retain_candidate_until_last_holder() -> Result<()> {
    for populated in [false, true] {
        let (_home, host, identity, source) = fixture(populated).await?;
        let expected = source.resident.capture(&request().capture)?;
        let expected_records = expected.records.len();
        drop((expected, source));
        assert_eq!(candidate_bytes(&host)?, 0);
        for entry in 0..3 {
            let holder: Box<dyn std::any::Any> = match entry {
                0 => Box::new(
                    host.local_agent_summary(&identity.agent_id)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?,
                ),
                1 => Box::new(
                    host.local_agent_lightweight_projection(&identity.agent_id)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?,
                ),
                _ => {
                    let read = host
                        .operator_agent_state_projection(&identity.agent_id, 40, 40)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?;
                    assert_eq!(
                        read.timers.len() + read.external_triggers.len(),
                        expected_records,
                    );
                    let zero = host
                        .operator_agent_state_projection(&identity.agent_id, 40, 0)
                        .await
                        .map_err(|error| anyhow!("{error:?}"))?;
                    assert!(zero.timers.is_empty());
                    assert_eq!(zero.external_triggers, read.external_triggers);
                    drop(zero);
                    Box::new(read)
                }
            };
            let held = candidate_bytes(&host)?;
            assert!(held > 0);
            let first = Arc::new(holder);
            let last = first.clone();
            drop(first);
            assert_eq!(candidate_bytes(&host)?, held);
            assert!(host.inner.runtimes.read().await.agents.is_empty());
            drop(last);
            assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        }
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn cold_capture_phase_ready_root_acquisition_keeps_original_publication_and_holder(
) -> Result<()> {
    let (_home, host) = test_host();
    let runtime = host.default_runtime().await?;
    let identity = host.ensure_default_agent_identity()?;
    let timer = runtime
        .schedule_timer(3_600_000, None, Some("ready capture 汉🦀".into()))
        .await?;
    drop(runtime.refresh_context_resources().await?);
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let baseline = candidate_bytes(&host)?;
    let read = host.acquire_cold_scheduling_query(&identity, 40).await?;
    assert!(read.timers.iter().any(|loan| loan == &timer));
    assert!(candidate_bytes(&host)? > baseline);
    let held = candidate_bytes(&host)?;
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    assert_eq!(candidate_bytes(&host)?, held);
    drop(last);
    assert_eq!(candidate_bytes(&host)?, baseline);
    assert_eq!(host.inner.runtimes.read().await.agents.len(), 1);
    drop(runtime);
    host.shutdown().await?;
    Ok(())
}
