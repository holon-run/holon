use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-lifecycle-audit-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn event_rows(runtime: &RuntimeHandle, kind: &str) -> Result<(usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events WHERE kind=?1),
         (SELECT COUNT(*) FROM current_read_members m JOIN audit_events e ON e.audit_event_id=m.record_id WHERE m.family='event' AND e.kind=?1),
         (SELECT COUNT(*) FROM current_event_native_births b JOIN audit_events e ON e.audit_event_id=b.event_id WHERE e.kind=?1)",
        [kind], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?)
}
fn selected_event(runtime: &RuntimeHandle, kind: &str) -> Result<AuditEvent> {
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("original published Current")?;
    let selected = root
        .current_reads
        .as_ref()
        .context("original Current facts")?
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    selected
        .events
        .iter()
        .filter_map(|record| match &record.fact {
            CurrentReadFact::Event(event) if event.kind == kind => Some(event.clone()),
            _ => None,
        })
        .max_by_key(|event| event.event_seq)
        .context("actual Lifecycle event omitted from published root")
}

#[tokio::test]
async fn actual_closure_observation_is_an_original_whole_audit_ack_and_rolls_back_members(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let closure = runtime.record_closure_decision_event(None).await?;
    let actual = selected_event(&runtime, "closure_decided")?;
    assert_eq!(
        actual.data,
        serde_json::json!({"agent_id":before.state.id,"closure":closure})
    );
    assert_eq!(event_rows(&runtime, "closure_decided")?, (1, 1, 1));
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(
        after.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    let receipts: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM input_mutation_receipts",
        [],
        |row| row.get(0),
    )?;
    runtime.runtime_db().connection()?.execute_batch(
        "CREATE TRIGGER fail_original_closure_member BEFORE INSERT ON current_read_members
         WHEN NEW.family='event' AND EXISTS(SELECT 1 FROM audit_events e WHERE e.audit_event_id=NEW.record_id AND e.kind='closure_decided')
         BEGIN SELECT RAISE(ABORT,'original closure member failure'); END;")?;
    let error = runtime
        .record_closure_decision_event(None)
        .await
        .err()
        .context("actual audit must roll back")?;
    assert!(format!("{error:#}").contains("original closure member failure"));
    assert_eq!(event_rows(&runtime, "closure_decided")?, (1, 1, 1));
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM input_mutation_receipts",
            [],
            |row| row.get::<_, usize>(0)
        )?,
        receipts
    );
    assert!(Arc::ptr_eq(
        &after,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    runtime.runtime_db().connection()?.execute_batch(
        "DROP TRIGGER fail_original_closure_member;
        ALTER TABLE audit_events RENAME TO original_lifecycle_audit_forbidden;",
    )?;
    let from_root = selected_event(&runtime, "closure_decided")?;
    assert_eq!(from_root, actual);
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("ALTER TABLE original_lifecycle_audit_forbidden RENAME TO audit_events;")?;
    Ok(())
}

#[tokio::test]
async fn actual_indefinite_sleep_warning_and_owned_duration_wake_publish_original_events(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    {
        let mut owner = runtime.begin_mutation().await?;
        owner.state.status = AgentStatus::AwakeIdle;
        owner.persist_state(&runtime.inner.storage).await?;
    }
    runtime
        .transition_to_sleep_with_runnable_override(None, false)
        .await?;
    let warning = selected_event(&runtime, "orphan_sleep_observed")?;
    assert_eq!(warning.data["severity"], "warning");
    assert_eq!(warning.data["agent_id"], runtime.instance_agent_id());
    assert_eq!(warning.data["evidence"].as_array().unwrap().len(), 8);
    assert_eq!(event_rows(&runtime, "orphan_sleep_observed")?, (1, 1, 1));
    runtime
        .transition_to_sleep_with_runnable_override(Some(1), false)
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let finished = runtime
                .inner
                .owned_task_handles
                .lock()
                .unwrap()
                .iter()
                .all(|task| task.is_finished());
            if finished {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await?;
    // Join only after the physical original wake continuation has completed;
    // closing the mutation registry must not preempt its accepted enqueue.
    runtime.join_idle_owned_tasks().await?;
    let emitted = selected_event(&runtime, "system_tick_emitted")?;
    assert_eq!(emitted.data["subsystem"], "sleep_duration");
    assert_eq!(emitted.data["sleep_wait"]["duration_ms"], 1);
    assert!(emitted.event_seq > warning.event_seq);
    assert_eq!(event_rows(&runtime, "system_tick_emitted")?, (1, 1, 1));
    let messages: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM messages WHERE agent_id=?1 AND kind='system_tick'",
        [runtime.instance_agent_id()],
        |row| row.get(0),
    )?;
    assert_eq!(messages, 1);
    Ok(())
}

#[tokio::test]
async fn actual_turn_end_observation_keeps_original_work_semantics_and_qualified_event(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let work = runtime
        .create_work_item("actual turn end observation".into(), None, None, Vec::new())
        .await?;
    crate::runtime::tests::support::bind_turn_to_work_item(&runtime, &work.id).await;
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let committed = runtime
        .maybe_commit_turn_end_work_item_transition()
        .await?
        .context("actual original Work must remain")?;
    assert_eq!(committed.id, work.id);
    assert_eq!(committed.state, WorkItemState::Open);
    assert!(runtime
        .agent_state()
        .await?
        .current_turn_work_item_id
        .is_none());
    let event = selected_event(&runtime, "work_item_turn_end_committed")?;
    assert_eq!(event.data["work_item_id"], work.id);
    assert_eq!(event.data["committed_state"], "open");
    assert_eq!(
        event_rows(&runtime, "work_item_turn_end_committed")?,
        (1, 1, 1)
    );
    assert_eq!(
        event_rows(&runtime, "work_item_turn_end_commit_skipped")?,
        (0, 0, 0)
    );
    Ok(())
}
