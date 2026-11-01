use super::*;
use crate::{
    types::{WaitConditionRecord, WaitConditionSummary},
    work_item_scheduling::WorkItemSchedulingProjection,
};

fn shape(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            2 + fields.len() * 2
                + fields.len().saturating_sub(1)
                + fields.values().map(shape).sum::<usize>()
        }
        serde_json::Value::Array(values) => {
            2 + values.len().saturating_sub(1) + values.iter().map(shape).sum::<usize>()
        }
        serde_json::Value::String(_) => 1,
        _ => 0,
    }
}

fn cloned(value: &impl serde::Serialize) -> Result<(u64, usize)> {
    let cells = shape(&serde_json::to_value(value)?);
    Ok((
        (serde_json::to_vec(value)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells + 16) * 8,
    ))
}

pub(super) fn summary_gates(wait: &WaitConditionRecord) -> Result<[(u64, usize); 3]> {
    // Old consuming conversion is an independent oracle for the borrowed
    // owning-field tuple; no production budget or borrowed conversion helper.
    let old = WaitConditionSummary::from(wait.clone());
    let fields = serde_json::json!([
        old.id,
        old.work_item_id,
        old.source,
        old.subject_ref,
        old.waiting_for,
        old.wake_sources,
        old.no_fallback_reason,
        old.continuation,
    ]);
    Ok([
        (
            std::mem::size_of::<(usize, &WaitConditionRecord)>() as u64 * 8,
            8,
        ),
        (std::mem::size_of::<WaitConditionSummary>() as u64 * 8, 8),
        cloned(&fields)?,
    ])
}

fn add(a: (u64, usize), b: (u64, usize)) -> (u64, usize) {
    (a.0 + b.0, a.1 + b.1)
}

#[tokio::test]
async fn wait_summary_each_gate_rejects_on_both_snapshot_paths_and_latches() -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
    let waits = host
        .runtime_db()
        .wait_conditions()
        .active_for_agent("web")?;
    assert_eq!(waits.len(), 1);
    let gates = summary_gates(&waits[0])?;
    let summaries = gates.into_iter().fold((0, 0), add);
    let queue = crate::storage::RuntimeReadModels::borrowed_for_agent(host.runtime_db(), "web")?
        .work_queue()?;
    assert_eq!(queue.waiting_for_operator.len(), 1);
    assert_eq!(queue.completed_recent.len(), 1);
    let mut displays = (0, 0);
    for row in queue
        .waiting_for_operator
        .iter()
        .chain(&queue.completed_recent)
    {
        let copy = cloned(row)?;
        displays = add(
            displays,
            add(
                (
                    std::mem::size_of::<WorkItemSchedulingProjection>() as u64 * 8,
                    8,
                ),
                copy,
            ),
        );
    }
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
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
            .fold(displays, |sum, text| {
                add(
                    sum,
                    ((text.len() + std::mem::size_of::<String>()) as u64 * 8, 8),
                )
            });
        let total = phase.finish()?;
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        // Only calibrate the unchanged SQL/Host prefix. Every new gate and all
        // downstream display/posture material are counted independently.
        let mut prefix = (
            total.0 - tail.0 - summaries.0,
            total.1 - tail.1 - summaries.1,
        );
        for (index, added) in gates.into_iter().enumerate() {
            let boundary = add(prefix, added);
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    boundary.0 - u64::from(bytes),
                    boundary.1 - usize::from(!bytes),
                )?;
                let original = phase.guard()?;
                {
                    let _entry = phase.enter();
                    assert!(Arc::ptr_eq(
                        &original,
                        &ResourceReadMaterial::active_guard()?.unwrap()
                    ));
                    let error = entry(&host, roster).unwrap_err();
                    assert_eq!(
                        error.to_string(),
                        [
                            "queue wait references admission",
                            "queue wait summary slots admission",
                            "queue wait summary clone admission",
                        ][index]
                    );
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
                        "context_resource_exhausted",
                    );
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                }
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                drop(original);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            prefix = boundary;
        }
    }
    Ok(())
}

#[tokio::test]
async fn wait_summary_keeps_original_holder_through_wire_failure_and_last_release() -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
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
                    return Err(serde::ser::Error::custom("wait-summary wire failure"));
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
            if let Ok(body) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&body)?;
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
