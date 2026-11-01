use super::*;
use std::borrow::Cow;

#[path = "cold_scheduler_canonical_phase_tests.rs"]
mod canonical_tests;

#[path = "cold_scheduler_activation_wait_tests.rs"]
mod activation_wait_tests;

fn shape(value: &serde_json::Value) -> usize {
    use serde_json::Value;
    match value {
        Value::Object(fields) => {
            2 + fields.len().saturating_sub(1)
                + fields.values().map(|value| 2 + shape(value)).sum::<usize>()
        }
        Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        Value::String(_) => 1,
        _ => 0,
    }
}

fn cost(
    identity: &AgentIdentityRecord,
    state: &AgentState,
    queue: &crate::work_item_scheduling::WorkQueueReadModel,
    borrowed: bool,
) -> Result<(u64, usize)> {
    let snapshot = snapshot_cost(state)?;
    let mut bytes = ((identity.agent_id.len() + 256) * 8) as u64 + snapshot.0;
    let mut records = 64 + snapshot.1;
    bytes += (queue.queued_runnable.len() * std::mem::size_of::<WorkItemRecord>() * 8) as u64;
    records += queue.queued_runnable.len() * 8;
    if borrowed {
        for record in queue
            .current
            .iter()
            .chain(queue.queued_runnable.iter().map(|item| &item.work_item))
        {
            let value = serde_json::to_value(record)?;
            let slots = shape(&value);
            bytes += (serde_json::to_vec(&value)?.len() as u64 * 4 + slots as u64 * 128 + 512) * 8;
            records += (slots + 16) * 8;
        }
    }
    Ok((bytes, records))
}

fn snapshot_cost(state: &AgentState) -> Result<(u64, usize)> {
    let value = serde_json::to_value((&state.id, &state.current_run_id))?;
    let slots = shape(&value);
    Ok((
        (serde_json::to_vec(&value)?.len() as u64 * 4 + slots as u64 * 128 + 512) * 8,
        (slots + 16) * 8,
    ))
}

#[tokio::test]
async fn cold_scheduler_work_standalone_cost_guard_and_queue_ownership() -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        standalone_cost_guard_and_queue_ownership(),
    )
    .await
    .context("cold scheduler standalone test exceeded its deadline")?
}

async fn standalone_cost_guard_and_queue_ownership() -> Result<()> {
    for populated in [false, true] {
        for borrowed in [false, true] {
            let (_home, host, identity, state, storage) = fixture(populated)?;
            if populated {
                let mut queued = WorkItemRecord::new(
                    &identity.agent_id,
                    "queued 汉🦀".repeat(90),
                    WorkItemState::Open,
                );
                queued.plan_status = WorkItemPlanStatus::Ready;
                assert!(host.runtime_db().work_items().insert_new(&queued)?);
            }
            let queue = storage.work_queue_with_observed_scheduling(&state, &[])?;
            let wire = queue_wire(&queue)?;
            let expected_cost = cost(&identity, &state, &queue, borrowed)?;
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
            assert_eq!(
                projection.queued_runnable_work_items,
                queue
                    .queued_runnable
                    .iter()
                    .map(|item| item.work_item.clone())
                    .collect::<Vec<_>>()
            );
            assert!(projection.waiting_work_item.is_none());
            assert_eq!(queue_wire(&queue)?, wire);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                baseline + expected_cost.0
            );
            drop((read.acquisition, read.timers, read.triggers));
            let first = Arc::new((projection, material));
            let last = first.clone();
            drop(first);
            assert_eq!(
                host.inner.input_evidence_budget.report()?.observed_bytes,
                baseline + expected_cost.0
            );
            assert_eq!(last.0.current_work_item, queue.current);
            drop(last);
            assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
            drop(read.reservations);
            assert!(host.inner.runtimes.read().await.agents.is_empty());
            host.shutdown().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn cold_scheduler_work_inherited_failure_retry_and_three_response_holders() -> Result<()> {
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        inherited_failure_retry_and_three_response_holders(),
    )
    .await
    .context("cold scheduler inherited retry test exceeded its deadline")?
}

async fn inherited_failure_retry_and_three_response_holders() -> Result<()> {
    let (_home, host, identity, state, storage) = fixture(true)?;
    let queue = storage.work_queue_with_observed_scheduling(&state, &[])?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    // Measure the full existing inherited adapter chain, including source gates;
    // the standalone test independently checks just this batch's new cost.
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
        let host_held = host.inner.input_evidence_budget.report()?.observed_bytes;
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
        // The synchronous read is complete; retry must not reacquire our own lock.
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
                host_held
            );
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, required.0);
            drop(held);
        } else {
            assert!(result.is_err());
            let latched = phase.finish().unwrap_err().to_string();
            let prefix = ledger.report()?.observed_bytes;
            let retry = host.acquire_cold_scheduling_query(&identity, 0).await?;
            let _entry = phase.enter();
            let error = host
                .cold_scheduler_with_observed_scheduling(
                    &identity,
                    &storage,
                    &state,
                    Cow::Borrowed(&queue),
                    retry.material,
                )
                .err()
                .context("inherited failure must not reset")?;
            assert_eq!(error.to_string(), latched);
            assert_eq!(ledger.report()?.observed_bytes, prefix);
            drop((retry.acquisition, retry.timers, retry.triggers));
            drop((_entry, phase));
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    }
    for api in 0..3 {
        let holder: Box<dyn std::any::Any> = match api {
            0 => {
                let read = host
                    .local_agent_summary(&identity.agent_id)
                    .await
                    .map_err(|error| anyhow!("{error:?}"))?;
                assert_eq!(read.agent, state);
                Box::new(read)
            }
            1 => {
                let read = host
                    .local_agent_lightweight_projection(&identity.agent_id)
                    .await
                    .map_err(|error| anyhow!("{error:?}"))?;
                assert_eq!(read.agent, state);
                assert_eq!(queue_wire(&read.work_queue)?, queue_wire(&queue)?);
                Box::new(read)
            }
            _ => {
                let read = host
                    .operator_agent_state_projection(&identity.agent_id, 0, 0)
                    .await
                    .map_err(|error| anyhow!("{error:?}"))?;
                assert_eq!(read.source, AgentStateProjectionSource::Storage);
                assert_eq!(read.agent.agent, state);
                assert_eq!(queue_wire(&read.agent.work_queue)?, queue_wire(&queue)?);
                Box::new(read)
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
