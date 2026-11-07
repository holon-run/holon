use super::*;
use crate::types::{WaitConditionKind, WaitConditionRecord, WaitConditionStatus, WakeSource};
use chrono::DateTime;

#[tokio::test]
async fn cold_activation_wait_source_real_callers_filter_failure_and_last_holders() -> Result<()> {
    tokio::time::timeout(std::time::Duration::from_secs(10), verify_callers())
        .await
        .context("cold activation wait caller test exceeded its deadline")?
}

async fn verify_callers() -> Result<()> {
    let (_home, host, identity, state, storage) = fixture(true)?;
    let owner_id = state
        .current_work_item_id
        .clone()
        .context("populated fixture must have a current WorkItem")?;
    let queue = storage.work_queue_with_observed_scheduling(&state, &[])?;
    let mut all = Vec::new();
    let now = "2026-10-20T15:00:00Z".parse::<DateTime<Utc>>()?;
    for (index, (status, kind)) in [
        (WaitConditionStatus::Active, WaitConditionKind::External),
        (WaitConditionStatus::Triggered, WaitConditionKind::Operator),
        (WaitConditionStatus::Resolved, WaitConditionKind::Task),
        (WaitConditionStatus::Resolved, WaitConditionKind::External),
        (WaitConditionStatus::Cancelled, WaitConditionKind::Task),
        (WaitConditionStatus::Expired, WaitConditionKind::System),
    ]
    .into_iter()
    .enumerate()
    {
        let row = WaitConditionRecord {
            id: format!("activation_{index}"),
            agent_id: identity.agent_id.clone(),
            // Keep Active and Triggered registrations under distinct valid owners.
            work_item_id: (index == 1).then(|| owner_id.clone()),
            status,
            kind,
            source: Some("source 汉🦀".into()),
            subject_ref: Some("subject".into()),
            waiting_for: "wait text".repeat(3),
            wake_sources: vec![WakeSource::SystemTick],
            continuation: Some(serde_json::json!({"nested": [{"text": "汉🦀"}]})),
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        };
        host.runtime_db().wait_conditions().upsert(&row)?;
        all.push(row);
    }
    assert_eq!(
        storage.latest_wait_conditions_for_agent(&identity.agent_id)?,
        all
    );
    let expected = all[..3].to_vec();
    for borrowed in [false, true] {
        assert!(ResourceReadMaterial::active_guard()?.is_none());
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
        assert_eq!(projection.activation_waits_for_test(), expected);
        assert!(ResourceReadMaterial::active_guard()?.is_none());
        let held_bytes = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(held_bytes > baseline + cost(&identity, &state, &queue, borrowed)?.0);
        drop((read.acquisition, read.timers, read.triggers));
        let first = Arc::new((projection, material));
        let last = first.clone();
        drop(first);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            held_bytes
        );
        drop(last);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        drop(read.reservations);
    }
    // Admit the preceding snapshot, then refuse the first source Vec growth.
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let header = (((identity.agent_id.len() + 256) * 8) as u64, 64);
    let snapshot = snapshot_cost(&state)?;
    let before_waits = (header.0 + snapshot.0, header.1 + snapshot.1);
    for bytes in [true, false] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope(&identity),
            if bytes {
                before_waits.0 + (4 * std::mem::size_of::<WaitConditionRecord>() * 8) as u64 - 1
            } else {
                64 * 1024 * 1024
            },
            if bytes {
                65536
            } else {
                before_waits.1 + 4 * 8 - 1
            },
        )?;
        let original = phase.guard()?;
        for _ in 0..2 {
            let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
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
            assert!(result.is_err());
            assert_eq!(ledger.report()?.observed_bytes, before_waits.0);
            assert!(phase.finish().is_err());
            drop(read.reservations);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, before_waits.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    for api in 0..3 {
        let holder: Box<dyn std::any::Any> = match api {
            0 => Box::new(
                host.local_agent_summary(&identity.agent_id)
                    .await
                    .map_err(|e| anyhow!("{e:?}"))?,
            ),
            1 => Box::new(
                host.local_agent_lightweight_projection(&identity.agent_id)
                    .await
                    .map_err(|e| anyhow!("{e:?}"))?,
            ),
            _ => Box::new(
                host.operator_agent_state_projection(&identity.agent_id, 0, 0)
                    .await
                    .map_err(|e| anyhow!("{e:?}"))?,
            ),
        };
        let bytes = host.inner.input_evidence_budget.report()?.observed_bytes;
        assert!(bytes > 0);
        let first = Arc::new(holder);
        let last = first.clone();
        drop(first);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            bytes
        );
        drop(last);
        assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
        assert!(host.inner.runtimes.read().await.agents.is_empty());
    }
    // Even an ineligible row must still decode and fail at the original source point.
    host.runtime_db().connection()?.execute(
        "UPDATE wait_conditions SET continuation_json='{' WHERE wait_condition_id='activation_5'",
        [],
    )?;
    let read = host.acquire_cold_scheduling_query(&identity, 0).await?;
    let ordinary = storage
        .latest_wait_conditions_for_agent(&identity.agent_id)
        .unwrap_err();
    let error = host
        .cold_scheduler_with_observed_scheduling(
            &identity,
            &storage,
            &state,
            Cow::Borrowed(&queue),
            read.material,
        )
        .err()
        .context("expired malformed wait must fail before filtering")?;
    assert_eq!(format!("{error:#}"), format!("{ordinary:#}"));
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    drop((
        read.acquisition,
        read.timers,
        read.triggers,
        read.reservations,
    ));
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}
