use super::*;
use crate::work_item_scheduling::WorkQueueReadModel;

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
async fn diagnostic_posture_honors_original_allowance() -> Result<()> {
    let (_home, host, _queue) = fixture().await?;
    super::assert_host_projection_limits(&host)
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
