use super::*;
use crate::domain::execution_protocol::{
    ExecutionProtocolState, WorkItemExecutionRecord, WorkItemExecutionState,
};

#[tokio::test]
async fn cold_canonical_map_real_caller_phase_limits_and_response_holders() -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), verify_real_callers())
        .await
        .context("cold canonical caller test exceeded its deadline")?
}

async fn verify_real_callers() -> Result<()> {
    let (_home, host, identity, state, storage) = fixture(true)?;
    let queue = storage.work_queue_with_observed_scheduling(&state, &[])?;
    let current = queue.current.as_ref().context("fixture current missing")?;
    let execution = ExecutionProtocolState {
        agent_id: identity.agent_id.clone(),
        attempts: Default::default(),
        work_items: [(
            current.id.clone(),
            WorkItemExecutionRecord {
                source_revision: current.revision,
                state: WorkItemExecutionState::Runnable {
                    generation: 31,
                    recovery_ref: None,
                },
            },
        )]
        .into_iter()
        .collect(),
        outcomes: Default::default(),
    };
    host.runtime_db()
        .transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &execution))?;
    assert_eq!(
        host.runtime_db()
            .transitions()
            .load_execution_protocol_state_if_initialized(&identity.agent_id)?,
        Some(execution)
    );
    // The standalone caller must create its explicit phase, not rely on TLS.
    for borrowed in [false, true] {
        let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let (projection, material) = host.cold_scheduler_with_observed_scheduling(
            &identity,
            &storage,
            &state,
            if borrowed {
                Cow::Borrowed(&queue)
            } else {
                Cow::Owned(queue.clone())
            },
            read.material,
        )?;
        assert_eq!(projection.current_work_item, queue.current);
        let signal = projection
            .work_reactivation_signal()
            .context("canonical runnable missing")?;
        assert_eq!(signal.work_item_id, current.id);
        let held = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > baseline + cost(&identity, &state, &queue, borrowed)?.0);
        drop((read.acquisition, read.timers, read.triggers));
        let first = Arc::new((projection, material));
        let last = first.clone();
        drop(first);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        drop(last);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        drop(read.reservations);
    }
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let measure = ResourceReadMaterial::new(&ledger, &scope(&identity))?;
    {
        let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
        let _entry = measure.enter();
        host.cold_scheduler_with_observed_scheduling(
            &identity,
            &storage,
            &state,
            Cow::Borrowed(&queue),
            read.material,
        )?;
    }
    let required = measure.finish()?;
    drop(measure);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for short in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(&identity),
            required.0 - u64::from(short == Some(true)),
            required.1 - usize::from(short == Some(false)),
        )?;
        let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
        let host_before = host.inner.input_evidence_budget.report()?.observed_bytes;
        let result = {
            let _entry = phase.enter();
            host.cold_scheduler_with_observed_scheduling(
                &identity,
                &storage,
                &state,
                Cow::Borrowed(&queue),
                read.material,
            )
        };
        drop((
            read.acquisition,
            read.timers,
            read.triggers,
            read.reservations,
        ));
        if short.is_none() {
            let held = result?;
            assert_eq!(phase.finish()?, required);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                host_before
            );
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, required.0);
            drop(held);
        } else {
            assert!(result.is_err());
            let latched = phase.finish().unwrap_err().to_string();
            let prefix = ledger.report()?.observed_bytes;
            let retry = host.acquire_cold_scheduling_query(&identity, 0).await?;
            let error = {
                let _entry = phase.enter();
                host.cold_scheduler_with_observed_scheduling(
                    &identity,
                    &storage,
                    &state,
                    Cow::Borrowed(&queue),
                    retry.material,
                )
                .err()
                .context("failed inherited phase reset")?
            };
            drop((
                retry.acquisition,
                retry.timers,
                retry.triggers,
                retry.reservations,
            ));
            assert_eq!(error.to_string(), latched);
            assert_eq!(ledger.report()?.observed_bytes, prefix);
            drop(phase);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    }
    for api in 0..3 {
        let holder: Box<dyn std::any::Any> = match api {
            0 => {
                let result = host
                    .local_agent_summary(&identity.agent_id)
                    .await
                    .map_err(|error| anyhow!("{error:?}"))?;
                assert_eq!(result.agent, state);
                Box::new(result)
            }
            1 => {
                let result = host
                    .local_agent_lightweight_projection(&identity.agent_id)
                    .await
                    .map_err(|error| anyhow!("{error:?}"))?;
                assert_eq!(result.agent, state);
                assert_eq!(queue_wire(&result.work_queue)?, queue_wire(&queue)?);
                Box::new(result)
            }
            _ => {
                let result = host
                    .operator_agent_state_projection(&identity.agent_id, 0, 0)
                    .await
                    .map_err(|error| anyhow!("{error:?}"))?;
                assert_eq!(result.source, AgentStateProjectionSource::Storage);
                assert_eq!(result.agent.agent, state);
                assert_eq!(queue_wire(&result.agent.work_queue)?, queue_wire(&queue)?);
                Box::new(result)
            }
        };
        let held = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held > 0);
        let first = Arc::new(holder);
        let last = first.clone();
        drop(first);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            held
        );
        drop(last);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        assert!(host.inner.runtimes.read().await.agents.is_empty());
    }
    host.shutdown().await?;
    Ok(())
}
