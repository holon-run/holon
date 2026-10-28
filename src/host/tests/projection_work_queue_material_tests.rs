use super::super::*;
use crate::{
    config::StateMemoryLimits,
    diagnostics::resource_io::ResourceReadMaterial,
    provider::StubProvider,
    runtime::{
        input_evidence_budget::{BudgetViewGuard, InputEvidenceBudgetLedger},
        resident_input_evidence::InputEvidenceScope,
    },
    types::{AgentSchedulingPosture, WorkItemPlanStatus, WorkItemRecord, WorkItemState},
};

#[path = "projection_work_queue_material_tests/diagnostics_material_tests.rs"]
mod diagnostics_material_tests;

#[path = "projection_work_queue_material_tests/wait_summary_material_tests.rs"]
mod wait_summary_material_tests;

#[path = "projection_work_queue_material_tests/latest_key_material_tests.rs"]
mod latest_key_material_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-work-queue".into(),
        identity_incarnation: 1,
    }
}

#[tokio::test]
async fn host_queue_display_clone_rejection_propagates_on_both_snapshot_paths() -> Result<()> {
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
    let (_home, host) = fixture().await?;
    let closed = host
        .runtime_db()
        .work_items()
        .latest("work_closed")?
        .unwrap();
    let last_display = crate::work_item_scheduling::derive_owned_work_item_scheduling(
        closed,
        false,
        false,
        &[],
        &|_| None,
    );
    let cells = shape(&serde_json::to_value(&last_display)?);
    let last_cost = (
        (serde_json::to_vec(&last_display)?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
        (cells + 16) * 8,
    );
    for roster in [false, true] {
        // Calibrate only the unchanged upstream prefix. Independently account
        // for the final display clone and the posture Strings after the queue.
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
            .fold((0u64, 0usize), |sum, text| {
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
        let prefix = (boundary.0 - last_cost.0, boundary.1 - last_cost.1);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { boundary.0 - 1 } else { total.0 },
                if bytes { total.1 } else { boundary.1 - 1 },
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                let error = entry(&host, roster).unwrap_err();
                assert!(
                    error.to_string().contains("queue display clone admission"),
                    "{error:#}"
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
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
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

async fn continuation_fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let (home, host) = items_fixture().await?;
    host.runtime_db().work_item_continuations().upsert(
        &crate::types::WorkItemContinuationFrame::new_on_completed(
            "web",
            "unlisted suspended 汉字\0🦀",
            "work_open",
            None,
        ),
    )?;
    Ok((home, host))
}

#[tokio::test]
async fn host_continuation_sql_slots_and_json_reject_on_both_paths_and_latch() -> Result<()> {
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
    let (_home, host) = continuation_fixture().await?;
    let summary = task_wait_summary_cost(&host)?;
    let frames = host
        .runtime_db()
        .work_item_continuations()
        .active_for_agent("web")?;
    assert_eq!(frames.len(), 1);
    let cells = shape(&serde_json::to_value(&frames[0])?);
    let gates = [
        (
            std::mem::size_of::<crate::types::WorkItemContinuationFrame>() as u64 * 4 * 8,
            4 * 8,
            "continuation slots admission",
        ),
        (
            (serde_json::to_vec(&frames[0])?.len() as u64 * 4 + cells as u64 * 128 + 512) * 8,
            (cells * 4 + 16) * 8,
            "continuation JSON admission",
        ),
    ];
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
        // The retained wait summary also follows the SQL gates.
        let tail = (
            (std::mem::size_of::<String>()
                + std::mem::size_of::<crate::work_item_scheduling::WorkItemSchedulingProjection>()
                + agent.scheduling_posture.reason.len()
                + std::mem::size_of::<String>()) as u64
                * 8
                + summary.0,
            24 + summary.1,
        );
        let total = phase.finish()?;
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let mut prefix = (
            total.0 - tail.0 - gates.iter().map(|gate| gate.0).sum::<u64>(),
            total.1 - tail.1 - gates.iter().map(|gate| gate.1).sum::<usize>(),
        );
        for (cost_bytes, cost_records, context) in gates {
            let boundary = (prefix.0 + cost_bytes, prefix.1 + cost_records);
            for bytes in [true, false] {
                let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
                let phase = ResourceReadMaterial::with_test_limits(
                    &ledger,
                    &scope(),
                    if bytes { boundary.0 - 1 } else { total.0 },
                    if bytes { total.1 } else { boundary.1 - 1 },
                )?;
                let holder = phase.guard()?;
                {
                    let _entry = phase.enter();
                    let error = entry(&host, roster).unwrap_err();
                    assert!(error.to_string().contains(context), "{error:#}");
                    assert!(error.downcast_ref::<std::io::Error>().is_some());
                    assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                    assert!(phase.finish().is_err());
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
                drop(holder);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
            prefix = boundary;
        }
    }
    Ok(())
}

#[tokio::test]
async fn host_continuation_sql_decode_error_releases_partial_snapshot_on_both_paths() -> Result<()>
{
    let (_home, host) = continuation_fixture().await?;
    host.runtime_db().connection()?.execute(
        "UPDATE work_item_continuations SET payload_json='{' WHERE agent_id='web'",
        [],
    )?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let holder = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = entry(&host, roster).unwrap_err();
            assert!(error
                .to_string()
                .contains("decoding work item continuation payload from runtime db"));
            assert!(error.downcast_ref::<serde_json::Error>().is_some());
            assert!(Arc::ptr_eq(
                ResourceReadMaterial::active_guard()?.as_ref().unwrap(),
                &holder
            ));
        }
        let cost = phase.finish()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(holder);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn host_queue_continuation_slots_reject_on_both_paths_and_latch() -> Result<()> {
    let (_home, host) = continuation_fixture().await?;
    let summary = task_wait_summary_cost(&host)?;
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
        let tail = (
            (agent.scheduling_posture.reason.len() + std::mem::size_of::<String>()) as u64 * 8
                + std::mem::size_of::<crate::work_item_scheduling::WorkItemSchedulingProjection>()
                    as u64
                    * 8
                + summary.0,
            16 + summary.1,
        );
        let total = phase.finish()?;
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let boundary = (total.0 - tail.0, total.1 - tail.1);
        let prefix = (
            boundary.0 - std::mem::size_of::<String>() as u64 * 8,
            boundary.1 - 8,
        );
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
                let error = entry(&host, roster).unwrap_err();
                assert!(
                    error
                        .to_string()
                        .contains("queue continuation slots admission"),
                    "{error:#}"
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
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
async fn host_queue_continuation_slots_keep_original_holder_through_wire_and_last_release(
) -> Result<()> {
    let (_home, host) = continuation_fixture().await?;
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
                return Err(serde::ser::Error::custom("continuation wire fixture"));
            }
            self.agent.serialize(serializer)
        }
    }
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
        drop((phase, original));
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: cost.0,
                fail,
            });
            if fail {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("continuation wire fixture"));
            } else {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&result?)?,
                    serde_json::to_value(&agent)?
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

async fn todo_fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let (home, host) = items_fixture().await?;
    let todos = vec![
        crate::types::TodoItem {
            text: "pending".into(),
            state: crate::types::TodoItemState::Pending,
        },
        crate::types::TodoItem {
            text: "selected 汉字 🦀 \0 \"{}[],:\\ ".repeat(8),
            state: crate::types::TodoItemState::InProgress,
        },
    ];
    host.runtime_db().connection()?.execute(
        "UPDATE work_items SET payload_json=json_set(payload_json, '$.todo_list', json(?1))
         WHERE work_item_id='work_open'",
        [serde_json::to_string(&todos)?],
    )?;
    Ok((home, host))
}

#[tokio::test]
async fn host_queue_current_todo_rejects_on_both_snapshot_paths_without_reset() -> Result<()> {
    let (_home, host) = todo_fixture().await?;
    let summary = task_wait_summary_cost(&host)?;
    let work = host.runtime_db().work_items().latest("work_open")?.unwrap();
    let selected = &work.todo_list[1];
    assert_eq!(selected.state, crate::types::TodoItemState::InProgress);
    // Two string fields: braces + two key cells + two colons + comma
    // + two value string cells, independent of punctuation inside the text.
    let todo = (
        (serde_json::to_vec(selected)?.len() as u64 * 4 + 9 * 128 + 512) * 8,
        (9 + 16) * 8,
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
        let tail = (
            (agent.scheduling_posture.reason.len() + std::mem::size_of::<String>()) as u64 * 8
                + summary.0,
            8 + summary.1,
        );
        let total = phase.finish()?;
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let boundary = (total.0 - tail.0, total.1 - tail.1);
        let prefix = (boundary.0 - todo.0, boundary.1 - todo.1);
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
                assert_eq!(error.to_string(), "queue current todo clone admission");
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(ResourceReadMaterial::active_guard().is_err());
                assert!(phase.finish().is_err());
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
async fn host_queue_current_todo_keeps_original_holder_through_wire_and_last_release() -> Result<()>
{
    let (_home, host) = todo_fixture().await?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::Idle
        );
        let cost = phase.finish()?;
        drop((phase, original));
        struct Checked<'a>(&'a AgentListEntry, &'a InputEvidenceBudgetLedger, u64, bool);
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.1.report().unwrap().observed_bytes, self.2);
                if self.3 {
                    return Err(serde::ser::Error::custom("todo wire fixture"));
                }
                serde::Serialize::serialize(self.0, serializer)
            }
        }
        for fail in [false, true] {
            let encoded = serde_json::to_vec(&Checked(&agent, &ledger, cost.0, fail));
            if fail {
                assert!(encoded
                    .unwrap_err()
                    .to_string()
                    .contains("todo wire fixture"));
            } else {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&encoded?)?,
                    serde_json::to_value(&agent)?
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

async fn items_fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let (home, host) = active_wait_fixture().await?;
    // A non-current task wait has no separate queue display copy or post-queue
    // active-wait reread, but retains its queue summary before the idle reason.
    host.runtime_db().connection()?.execute_batch(
        "DELETE FROM work_items WHERE work_item_id='work_closed';
         UPDATE agent_states SET payload_json=json_set(payload_json, '$.current_work_item_id', NULL)
         WHERE agent_id='web';
         UPDATE wait_conditions SET kind='task' WHERE wait_condition_id='wait_projection';",
    )?;
    Ok((home, host))
}

fn task_wait_summary_cost(host: &RuntimeHost) -> Result<(u64, usize)> {
    let waits = host
        .runtime_db()
        .wait_conditions()
        .active_for_agent("web")?;
    assert_eq!(waits.len(), 1);
    assert_eq!(waits[0].work_item_id.as_deref(), Some("work_open"));
    assert_eq!(waits[0].kind, crate::types::WaitConditionKind::Task);
    // Count all three downstream gates with the independent old-conversion
    // JSON-tree oracle, not the production admission/conversion helper.
    Ok(wait_summary_material_tests::summary_gates(&waits[0])?
        .into_iter()
        .fold((0, 0), |sum, cost| (sum.0 + cost.0, sum.1 + cost.1)))
}

#[tokio::test]
async fn host_queue_items_slots_reject_on_both_snapshot_paths_and_preserve_failure() -> Result<()> {
    let (_home, host) = items_fixture().await?;
    let summary = task_wait_summary_cost(&host)?;
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
        let tail = (
            (agent.scheduling_posture.reason.len() + std::mem::size_of::<String>()) as u64 * 8
                + summary.0,
            8 + summary.1,
        );
        let slots = (
            std::mem::size_of::<crate::work_item_scheduling::WorkItemSchedulingProjection>() as u64
                * 8,
            8,
        );
        let total = phase.finish()?;
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        let boundary = (total.0 - tail.0, total.1 - tail.1);
        let prefix = (boundary.0 - slots.0, boundary.1 - slots.1);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes { boundary.0 - 1 } else { total.0 },
                if bytes { total.1 } else { boundary.1 - 1 },
            )?;
            let holder = phase.guard()?;
            {
                let _entry = phase.enter();
                let error = entry(&host, roster).unwrap_err();
                assert!(
                    error.to_string().contains("queue items slots admission"),
                    "{error:#}"
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
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
            drop(holder);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[tokio::test]
async fn host_queue_items_slots_keep_original_holder_through_wire_success_and_error() -> Result<()>
{
    let (_home, host) = items_fixture().await?;
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
                return Err(serde::ser::Error::custom("items wire fixture"));
            }
            self.agent.serialize(serializer)
        }
    }
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
        drop((phase, original));
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: cost.0,
                fail,
            });
            if fail {
                assert!(result
                    .unwrap_err()
                    .to_string()
                    .contains("items wire fixture"));
            } else {
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&result?)?,
                    serde_json::to_value(&agent)?
                );
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop(agent);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(retained);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn host_queue_current_id_borrow_preserves_focus_and_wire_holder() -> Result<()> {
    let (_home, host) = fixture().await?;
    let id = "focus 汉字\0🦀".repeat(32);
    let mut work = host.runtime_db().work_items().latest("work_open")?.unwrap();
    work.id = id.clone();
    let mut state = AgentState::new("web");
    state.status = AgentStatus::AwakeIdle;
    state.current_work_item_id = Some(id.clone());
    let connection = host.runtime_db().connection()?;
    connection.execute(
        "UPDATE work_items SET work_item_id=?1, payload_json=?2 WHERE work_item_id='work_open'",
        rusqlite::params![id, serde_json::to_string(&work)?],
    )?;
    connection.execute(
        "UPDATE agent_states SET payload_json=?1 WHERE agent_id='web'",
        [serde_json::to_string(&state)?],
    )?;
    drop(connection);
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::HasRunnableWork
        );
        assert_eq!(
            agent.scheduling_posture.work_item_id.as_deref(),
            Some(id.as_str())
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
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
                    return Err(serde::ser::Error::custom("current-id wire failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: cost.0,
                fail,
            });
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&bytes)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
            }
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

async fn trigger_fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let (home, host) = continuation_fixture().await?;
    let time = "2026-10-01T00:01:00Z".parse()?;
    let (mut trigger, _, _) = host
        .runtime_db()
        .external_triggers()
        .ensure_default_for_agent(
            "web",
            crate::types::CallbackDeliveryMode::WakeHint,
            time,
            "trigger-material-fixture".into(),
            "fixture-only-token".into(),
        )?;
    trigger.last_delivered_at = Some(time);
    trigger.delivery_count = 1;
    host.runtime_db().external_triggers().upsert(&trigger)?;
    assert_eq!(
        host.runtime_db()
            .external_triggers()
            .latest_for_agent("web")?
            .iter()
            .filter(
                |row| row.status == crate::types::ExternalTriggerStatus::Active
                    && row.last_delivered_at.is_some()
            )
            .count(),
        1
    );
    Ok((home, host))
}

#[tokio::test]
async fn host_queue_trigger_slots_reject_on_both_paths_without_partial_snapshot_or_reset(
) -> Result<()> {
    let (_home, host) = trigger_fixture().await?;
    let summary = task_wait_summary_cost(&host)?;
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
        let index_cost = (
            std::mem::size_of::<(String, chrono::DateTime<chrono::Utc>, usize)>() as u64 * 8,
            8,
        );
        // After the trigger gate: suspended ID, item, wait summary, idle reason.
        let tail = (
            (std::mem::size_of::<String>()
                + std::mem::size_of::<crate::work_item_scheduling::WorkItemSchedulingProjection>()
                + agent.scheduling_posture.reason.len()
                + std::mem::size_of::<String>()) as u64
                * 8
                + summary.0,
            24 + summary.1,
        );
        let total = phase.finish()?;
        let prefix = (
            total.0 - index_cost.0 - tail.0,
            total.1 - index_cost.1 - tail.1,
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &phase.guard()?));
        drop((agent, retained, phase));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        for bytes in [true, false] {
            let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                if bytes {
                    prefix.0 + index_cost.0 - 1
                } else {
                    total.0
                },
                if bytes {
                    total.1
                } else {
                    prefix.1 + index_cost.1 - 1
                },
            )?;
            let original = phase.guard()?;
            {
                let _entry = phase.enter();
                let error = entry(&host, roster).unwrap_err();
                assert!(
                    error.to_string().contains("queue trigger slots admission"),
                    "{error:#}"
                );
                assert!(error.downcast_ref::<std::io::Error>().is_some());
                assert_eq!(ledger.report()?.observed_bytes, prefix.0);
                assert!(phase.finish().is_err());
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
async fn host_queue_trigger_index_keeps_original_holder_through_wire_and_last_release() -> Result<()>
{
    let (_home, host) = trigger_fixture().await?;
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
                return Err(serde::ser::Error::custom("trigger-index wire fixture"));
            }
            self.agent.serialize(serializer)
        }
    }
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            let result = entry(&host, roster)?;
            assert!(Arc::ptr_eq(
                &original,
                &ResourceReadMaterial::active_guard()?.unwrap()
            ));
            result
        };
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
        let last = retained.as_ref().unwrap().clone();
        drop((phase, original));
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: cost.0,
                fail,
            });
            assert_eq!(result.is_err(), fail);
            if let Ok(wire) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&wire)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf()))?;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done")))?;
    host.create_named_agent("web", None).await?;
    let mut state = AgentState::new("web");
    state.status = AgentStatus::AwakeIdle;
    host.runtime_db().connection()?.execute(
        "INSERT OR REPLACE INTO agent_states (agent_id, status, updated_at, payload_json)
         VALUES ('web', 'awake_idle', '2026-02-01T00:00:00.000Z', ?1)",
        [serde_json::to_string(&state)?],
    )?;
    for terminal in [false, true] {
        let mut item = WorkItemRecord::new(
            "web",
            "queue material 汉字 \"{}\"",
            if terminal {
                WorkItemState::Completed
            } else {
                WorkItemState::Open
            },
        );
        item.id = if terminal { "work_closed" } else { "work_open" }.into();
        item.plan_status = WorkItemPlanStatus::Ready;
        item.created_at = "2026-10-01T00:00:00Z".parse()?;
        item.updated_at = item.created_at;
        assert!(host.runtime_db().work_items().insert_new(&item)?);
    }
    Ok((home, host))
}

fn entry(
    host: &RuntimeHost,
    roster: bool,
) -> Result<(AgentListEntry, Option<Arc<BudgetViewGuard>>)> {
    if roster {
        let mut snapshot = host.agent_roster_snapshot()?;
        assert_eq!(snapshot.agents.len(), 1);
        Ok((snapshot.agents.pop().unwrap().agent, snapshot._material))
    } else {
        let snapshot = host.agent_projection_snapshot("web")?.unwrap();
        Ok((snapshot.agent, snapshot._material))
    }
}

#[tokio::test]
async fn host_queue_sql_material_keeps_original_holder_through_wire_and_last_release() -> Result<()>
{
    let (_home, host) = fixture().await?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::HasRunnableWork
        );
        assert_eq!(
            agent.scheduling_posture.work_item_id.as_deref(),
            Some("work_open")
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
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
                    return Err(serde::ser::Error::custom("queue-wire failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: cost.0,
                fail,
            });
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&bytes)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn host_queue_sql_bad_non_open_payload_propagates_without_partial_snapshot_or_reset(
) -> Result<()> {
    let (_home, host) = fixture().await?;
    host.runtime_db().connection()?.execute(
        "UPDATE work_items SET payload_json=replace(payload_json, '2026-10-01', 'x026-10-01')
         WHERE work_item_id='work_closed'",
        [],
    )?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            let error = entry(&host, roster).unwrap_err();
            assert_eq!(
                error.to_string(),
                "decoding work item payload from runtime db"
            );
            assert!(error.downcast_ref::<serde_json::Error>().is_some());
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
        }
        let cost = phase.finish()?;
        assert!(cost.0 > (scope().agent_id.len() as u64 + 256) * 8);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

async fn active_wait_fixture() -> Result<(tempfile::TempDir, RuntimeHost)> {
    let (home, host) = fixture().await?;
    host.runtime_db().connection()?.execute(
        "UPDATE agent_states SET payload_json=json_set(payload_json, '$.current_work_item_id', 'work_open')
         WHERE agent_id='web'",
        [],
    )?;
    let now = "2026-10-01T00:00:00Z".parse()?;
    host.runtime_db()
        .wait_conditions()
        .upsert(&crate::types::WaitConditionRecord {
            id: "wait_projection".into(),
            agent_id: "web".into(),
            work_item_id: Some("work_open".into()),
            status: crate::types::WaitConditionStatus::Active,
            kind: crate::types::WaitConditionKind::Operator,
            source: Some("source".into()),
            subject_ref: Some("subject 汉字".into()),
            waiting_for: "operator \"[]{}\"".into(),
            wake_sources: vec![crate::types::WakeSource::OperatorInput],
            continuation: Some(serde_json::json!({"nested": ["汉字", null]})),
            created_at: now,
            updated_at: now,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: Some("turn".into()),
            trigger_message_id: None,
            triggered_at: None,
        })?;
    Ok((home, host))
}

#[tokio::test]
async fn host_active_wait_sql_material_keeps_original_holder_through_wire_and_last_release(
) -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::WaitingForOperator
        );
        assert_eq!(
            agent.scheduling_posture.work_item_id.as_deref(),
            Some("work_open")
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
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
                    return Err(serde::ser::Error::custom("active-wait-wire failure"));
                }
                serde::Serialize::serialize(self.agent, serializer)
            }
        }
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked {
                agent: &agent,
                ledger: &ledger,
                bytes: cost.0,
                fail,
            });
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&bytes)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
            }
            assert_eq!(ledger.report()?.observed_bytes, cost.0);
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn host_wait_scope_filters_terminal_missing_owners_with_original_holder() -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
    let mut wait = host
        .runtime_db()
        .wait_conditions()
        .active_for_agent("web")?
        .remove(0);
    wait.work_item_id = Some("work_closed".into());
    // An active wait cannot be retargeted by upsert; seed the historical owner directly.
    assert_eq!(
        host.runtime_db().connection()?.execute(
            "UPDATE wait_conditions SET work_item_id='work_closed',
             payload_json=json_set(payload_json, '$.work_item_id', 'work_closed')
             WHERE wait_condition_id='wait_projection'",
            [],
        )?,
        1
    );
    wait.id = "wait_missing".into();
    wait.work_item_id = Some("absent".into());
    host.runtime_db().wait_conditions().upsert(&wait)?;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        let (agent, retained) = {
            let _entry = phase.enter();
            entry(&host, roster)?
        };
        assert_eq!(
            agent.scheduling_posture.posture,
            AgentSchedulingPosture::HasRunnableWork
        );
        assert_eq!(
            agent.scheduling_posture.work_item_id.as_deref(),
            Some("work_open")
        );
        assert!(Arc::ptr_eq(retained.as_ref().unwrap(), &original));
        let cost = phase.finish()?;
        let last = retained.as_ref().unwrap().clone();
        drop((phase, original));
        struct Checked<'a>(&'a AgentListEntry, &'a InputEvidenceBudgetLedger, u64, bool);
        impl serde::Serialize for Checked<'_> {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                assert_eq!(self.1.report().unwrap().observed_bytes, self.2);
                if self.3 {
                    return Err(serde::ser::Error::custom("scope-wire failure"));
                }
                serde::Serialize::serialize(self.0, serializer)
            }
        }
        for fail in [false, true] {
            let result = serde_json::to_vec(&Checked(&agent, &ledger, cost.0, fail));
            assert_eq!(result.is_err(), fail);
            if let Ok(bytes) = result {
                let decoded: AgentListEntry = serde_json::from_slice(&bytes)?;
                assert_eq!(decoded.scheduling_posture, agent.scheduling_posture);
            }
        }
        drop((agent, retained));
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(last);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn host_wait_scope_lookup_error_outside_queue_scope_has_no_partial_snapshot() -> Result<()> {
    let (_home, host) = active_wait_fixture().await?;
    host.runtime_db().connection()?.execute_batch(
        "UPDATE work_items SET agent_id='other' WHERE work_item_id='work_closed';
         UPDATE wait_conditions SET work_item_id='work_closed' WHERE wait_condition_id='wait_projection';
         UPDATE work_items SET payload_json=replace(payload_json, '2026-10-01', 'x026-10-01')
         WHERE work_item_id='work_closed';"
    )?;
    assert!(host
        .runtime_db()
        .work_items()
        .latest_non_open_for_agent("web", 128)?
        .is_empty());
    let expected = host
        .runtime_db()
        .work_items()
        .latest_many(&["work_closed".into()])
        .unwrap_err();
    assert!(expected.downcast_ref::<serde_json::Error>().is_some());
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            let actual = entry(&host, roster).unwrap_err();
            assert_eq!(format!("{actual:#}"), format!("{expected:#}"));
            assert!(actual.downcast_ref::<serde_json::Error>().is_some());
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
        }
        let cost = phase.finish()?;
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[tokio::test]
async fn host_active_wait_sql_decode_error_returns_no_partial_snapshot_or_new_phase() -> Result<()>
{
    let (_home, host) = active_wait_fixture().await?;
    host.runtime_db().connection()?.execute(
        "UPDATE wait_conditions SET wake_sources_json='[' WHERE wait_condition_id='wait_projection'",
        [],
    )?;
    let expected = host
        .runtime_db()
        .wait_conditions()
        .active_for_agent("web")
        .unwrap_err()
        .to_string();
    assert!(expected.starts_with("reading wait conditions: "));
    assert!(expected.contains("parsing wake_sources_json"));
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let phase = ResourceReadMaterial::new(&ledger, &scope())?;
        let original = phase.guard()?;
        {
            let _entry = phase.enter();
            assert_eq!(entry(&host, roster).unwrap_err().to_string(), expected);
            assert!(Arc::ptr_eq(
                &ResourceReadMaterial::active_guard()?.unwrap(),
                &original
            ));
        }
        let cost = phase.finish()?;
        assert!(cost.0 > (scope().agent_id.len() as u64 + 256) * 8);
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, cost.0);
        drop(original);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
