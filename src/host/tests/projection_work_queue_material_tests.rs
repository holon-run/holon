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

#[cfg(holon_lib_test_shard = "d")]
#[path = "projection_work_queue_material_tests/diagnostics_material_tests.rs"]
mod diagnostics_material_tests;

#[path = "projection_work_queue_material_tests/wait_summary_material_tests.rs"]
mod wait_summary_material_tests;

#[cfg(holon_lib_test_shard = "d")]
#[path = "projection_work_queue_material_tests/latest_key_material_tests.rs"]
mod latest_key_material_tests;

fn scope() -> InputEvidenceScope {
    InputEvidenceScope {
        agent_id: "projection-work-queue".into(),
        identity_incarnation: 1,
    }
}

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn posture_with_completed_display_history_honors_original_allowance() -> Result<()> {
    let (_home, host) = fixture().await?;
    assert_host_projection_limits(&host)
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn posture_with_continuations_honors_original_allowance() -> Result<()> {
    let (_home, host) = continuation_fixture().await?;
    assert_host_projection_limits(&host)
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn posture_with_continuation_queue_honors_original_allowance() -> Result<()> {
    let (_home, host) = continuation_fixture().await?;
    assert_host_projection_limits(&host)
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn posture_with_todos_honors_original_allowance() -> Result<()> {
    let (_home, host) = todo_fixture().await?;
    assert_host_projection_limits(&host)
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn posture_with_task_waits_honors_original_allowance() -> Result<()> {
    let (_home, host) = items_fixture().await?;
    assert_host_projection_limits(&host)
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn posture_with_trigger_deliveries_honors_original_allowance() -> Result<()> {
    let (_home, host) = trigger_fixture().await?;
    assert_host_projection_limits(&host)
}

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn host_posture_ignores_malformed_completed_display_history() -> Result<()> {
    let (_home, host) = fixture().await?;
    let before = [entry(&host, false)?.0, entry(&host, true)?.0];
    host.runtime_db().connection()?.execute_batch(
        "UPDATE work_items SET payload_json='not-json' WHERE work_item_id='work_closed';",
    )?;
    // Roster posture no longer owns completed display history. Full Work
    // readers still reject its malformed body.
    assert!(host
        .runtime_db()
        .work_items()
        .latest("work_closed")
        .is_err());
    for (roster, expected) in [false, true].into_iter().zip(before) {
        assert_eq!(
            serde_json::to_value(entry(&host, roster)?.0)?,
            serde_json::to_value(expected)?
        );
    }
    assert_host_projection_limits(&host)
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
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

#[cfg(holon_lib_test_shard = "d")]
#[tokio::test]
async fn host_posture_does_not_read_a_foreign_completed_wait_owner() -> Result<()> {
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
            let (actual, retained) = entry(&host, roster)?;
            assert_eq!(actual.identity.agent_id, "web");
            drop((actual, retained));
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

#[cfg(holon_lib_test_shard = "d")]
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

// Integration checks the projection's whole original allowance. Exact queue
// primitive gates remain in storage/resident tests; posture no longer acquires
// the former full display queue or its incidental clone sequence.
fn assert_host_projection_limits(host: &RuntimeHost) -> Result<()> {
    use crate::diagnostics::resource_io::assert_retryable_resource_error;
    for roster in [false, true] {
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let measured = ResourceReadMaterial::new(&ledger, &scope())?;
        let value = measured.read(|| entry(host, roster))?;
        let (bytes, records) = measured.finish()?;
        drop((value, measured));
        assert_eq!(ledger.report()?.observed_bytes, 0);
        for byte_axis in [true, false] {
            let phase = ResourceReadMaterial::with_test_limits(
                &ledger,
                &scope(),
                bytes - u64::from(byte_axis),
                records - usize::from(!byte_axis),
            )?;
            let _original = phase.guard()?;
            let error = phase.read(|| entry(host, roster)).unwrap_err();
            assert_retryable_resource_error(&error);
            assert!(phase.finish().is_err());
            let observed = ledger.report()?.observed_bytes;
            let retry = phase.read(|| entry(host, roster)).unwrap_err();
            assert_retryable_resource_error(&retry);
            assert_eq!(ledger.report()?.observed_bytes, observed);
            drop(phase);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
