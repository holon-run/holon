use super::*;
use crate::work_item_scheduling::{WorkItemSchedulingProjection, WorkQueueReadModel};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost, WorkQueueReadModel)> {
    let (home, host) = active_wait_fixture().await?;
    // Same-owner wait registration replaces the old wait. Exercise the
    // current+yielded diagnostic without bypassing that ownership invariant.
    host.runtime_db().work_item_continuations().upsert(
        &crate::types::WorkItemContinuationFrame::new_on_completed(
            "web",
            "work_open",
            "work_closed",
            None,
        ),
    )?;
    let queue = crate::storage::RuntimeReadModels::borrowed_for_agent(host.runtime_db(), "web")?
        .work_queue()?;
    assert_eq!(queue.items.len(), 2);
    assert_eq!(queue.yielded.len(), 1);
    assert!(queue.yielded[0].is_current);
    assert!(queue.waiting_for_operator.is_empty());
    assert_eq!(queue.completed_recent.len(), 1);
    assert_eq!(
        queue.yielded[0].diagnostics,
        ["yielded_work_item_marked_current"]
    );
    assert!(queue.completed_recent[0].diagnostics.is_empty());
    Ok((home, host, queue))
}

#[tokio::test]
async fn diagnostics_rejects_on_both_snapshot_paths_and_latches() -> Result<()> {
    fn cells(value: &serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(fields) => {
                2 + fields.len() * 2
                    + fields.len().saturating_sub(1)
                    + fields.values().map(cells).sum::<usize>()
            }
            serde_json::Value::Array(values) => {
                2 + values.len().saturating_sub(1) + values.iter().map(cells).sum::<usize>()
            }
            serde_json::Value::String(_) => 1,
            _ => 0,
        }
    }
    let (_home, host, queue) = fixture().await?;
    let mut display = (0u64, 0usize);
    // Wait summaries now follow diagnostic admission on this same phase.
    for wait in host
        .runtime_db()
        .wait_conditions()
        .active_for_agent("web")?
    {
        for next in super::wait_summary_material_tests::summary_gates(&wait)? {
            display.0 += next.0;
            display.1 += next.1;
        }
    }
    for row in queue.yielded.iter().chain(&queue.completed_recent) {
        // Two one-element display windows follow diagnostic admission.
        let shape = cells(&serde_json::to_value(row)?);
        display.0 += (std::mem::size_of::<WorkItemSchedulingProjection>() as u64
            + serde_json::to_vec(row)?.len() as u64 * 4
            + shape as u64 * 128
            + 512)
            * 8;
        display.1 += (1 + shape + 16) * 8;
    }
    let diagnostic = (
        ("yielded_work_item_marked_current".len() + std::mem::size_of::<String>()) as u64 * 8,
        16,
    );
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::Idle
        );
        assert_eq!(
            agent.scheduling_posture.reason,
            "no queued input, active turn, runnable work, or active waits"
        );
        let posture = &agent.scheduling_posture;
        let tail = std::iter::once(posture.reason.as_str())
            .chain(
                [
                    posture.work_item_id.as_deref(),
                    posture.task_id.as_deref(),
                    posture.run_id.as_deref(),
                ]
                .into_iter()
                .flatten(),
            )
            .fold(display, |sum, text| {
                (
                    sum.0 + (text.len() + std::mem::size_of::<String>()) as u64 * 8,
                    sum.1 + 8,
                )
            });
        let total = phase.finish()?;
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let boundary = (total.0 - tail.0, total.1 - tail.1);
        let prefix = (boundary.0 - diagnostic.0, boundary.1 - diagnostic.1);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { boundary.0 - 1 } else { total.0 },
                if bytes { total.1 } else { boundary.1 - 1 },
            )?;
            let original = phase.guard()?;
            {
                let _entry = phase.enter();
                assert!(Arc::ptr_eq(
                    &ResourceReadMaterial::active_guard()?.unwrap(),
                    &original
                ));
                let error = entry(&host, roster).unwrap_err();
                assert_eq!(error.to_string(), "queue diagnostics admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
                assert!(ResourceReadMaterial::active_guard().is_err());
                let retry = entry(&host, roster).unwrap_err();
                assert_eq!(
                    retry
                        .downcast_ref::<crate::runtime_error::RuntimeError>()
                        .unwrap()
                        .descriptor()
                        .code,
                    "context_resource_exhausted"
                );
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            }
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, prefix.0);
            drop(original);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn diagnostics_keeps_original_holder_through_wire_and_last_release() -> Result<()> {
    let (_home, host, _queue) = fixture().await?;
    for roster in [false, true] {
        let expected = entry(&host, roster)?.0;
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(agent.scheduling_posture, expected.scheduling_posture);
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::Idle
        );
        assert_eq!(
            agent.scheduling_posture.reason,
            "no queued input, active turn, runnable work, or active waits"
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let total = phase.finish()?;
        let last = retained.as_ref().unwrap().clone();
        drop((phase, original));
        struct Checked<'a> {
            agent: &'a AgentListEntry,
            ledger: &'a InputEvidenceBudgetLedger,
            bytes: u64,
            fail: bool,
        }
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.ledger.report().unwrap().observed_bytes, self.bytes);
                if self.fail {
                    return Err(serde::ser::Error::custom("diagnostics-wire failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: total.0,
                fail,
            });
            assert_eq!(result.is_err(), fail);
            if let Ok(wire) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&wire)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
            }
            assert_eq!(ledger.report()?.observed_bytes, total.0);
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, total.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
