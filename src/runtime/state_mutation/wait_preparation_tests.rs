use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};
use crate::types::{CallbackDeliveryMode, WaitConditionStatus, WakeSource};
fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "wait-scheduling",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "wait-scheduling".into(),
        context_config(),
    )
}
async fn stop_original_timer_loop(runtime: &RuntimeHandle, id: &str) -> Result<()> {
    for lease in runtime.inner.control_cancellations.timer_for(id) {
        lease.cancel();
        lease.join().await?;
    }
    Ok(())
}
fn deny_body_tables(runtime: &RuntimeHandle, deny: bool) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(if deny {
            "ALTER TABLE timers RENAME TO wait_timer_body_forbidden; ALTER TABLE timer_wakes RENAME TO wait_wake_body_forbidden; ALTER TABLE external_triggers RENAME TO wait_trigger_body_forbidden"
        } else {
            "ALTER TABLE wait_trigger_body_forbidden RENAME TO external_triggers; ALTER TABLE wait_wake_body_forbidden RENAME TO timer_wakes; ALTER TABLE wait_timer_body_forbidden RENAME TO timers"
        })?;Ok(())
    })
}
#[tokio::test]
async fn completed_timer_wait_preparation_uses_published_pending_and_retains_guard_until_original_ack(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let mut timer = runtime
        .schedule_timer(3_600_000, None, Some("pending original wake".into()))
        .await?;
    stop_original_timer_loop(&runtime, &timer.id).await?;
    runtime.fire_timer_record(&mut timer).await?;
    assert_eq!(timer.status, TimerStatus::Completed);
    let mut owner = runtime.begin_mutation().await?;
    let expected = owner
        .prepare_timer_mutation_read(&timer.id, Some(&timer), false)
        .await?
        .context("Timer missing")?;
    let message_id = expected
        .pending
        .as_ref()
        .context("pending Wake missing")?
        .message_id
        .clone();
    drop(expected);
    let token = owner.capture_published_root()?.token;
    deny_body_tables(&runtime, true)?;
    let prepared = owner
        .prepare_wait_for_outcome_with_id(
            runtime.instance_agent_id(),
            None,
            WaitForWakeKind::Timer,
            Some(format!(" {} ", timer.id)),
            "original completed Timer".into(),
            None,
            None,
        )
        .await;
    deny_body_tables(&runtime, false)?;
    let PrepareWaitForOutcome::Prepared(plan) = prepared? else {
        anyhow::bail!("Timer must prepare original Wait")
    };
    assert_eq!(
        plan.registration.condition.status,
        WaitConditionStatus::Triggered
    );
    assert_eq!(
        plan.registration.condition.trigger_message_id.as_deref(),
        Some(message_id.as_str())
    );
    assert_eq!(
        plan.command.timer_wake.as_ref().unwrap().message_id,
        message_id
    );
    assert_eq!(owner.capture_published_root()?.token, token);
    let commit = owner
        .commit_wait_transition(
            crate::ids::audit_event_id(),
            plan.command.clone(),
            plan.execution_protocol.clone(),
            plan.expected_task.clone(),
        )
        .await?;
    drop(owner);
    runtime.apply_transition_commit(commit).await;
    let (status, receipt_count, queue_status): (String, usize, String) =
        runtime.runtime_db().connection()?.query_row(
            "SELECT status,(SELECT COUNT(*) FROM input_mutation_receipts),
                (SELECT status FROM queue_entries WHERE message_id=?2)
         FROM timer_wakes WHERE timer_id=?1 AND message_id=?2",
            rusqlite::params![timer.id, message_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    // Wait registration only binds the observed pending tuple. Incorporation
    // belongs to the later actual Queue claim, which this test has not run.
    assert_eq!(status, "pending");
    assert_eq!(queue_status, "queued");
    assert!(receipt_count > 0);
    drop(plan);
    Ok(())
}
#[tokio::test]
async fn external_wait_default_and_revoked_empty_use_one_root_without_trigger_body_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    // Required Trigger88 producer: this is an actual original whole ACK, not a raw fixture insert.
    let trigger = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    for active in [true, false] {
        if !active {
            runtime
                .revoke_external_trigger(&trigger.external_trigger_id)
                .await?;
        }
        let mut owner = runtime.begin_mutation().await?;
        let request = SchedulingCaptureRequest {
            include_live: true,
            history_limit: 0,
            required_keys: Default::default(),
        };
        owner.prepare_owner_scheduling_request(&request).await?;
        let token = owner.capture_published_root()?.token;
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        deny_body_tables(&runtime, true)?;
        let result = owner
            .prepare_wait_for_outcome_with_id(
                runtime.instance_agent_id(),
                None,
                WaitForWakeKind::External,
                Some("original external condition".into()),
                "original external Wait".into(),
                None,
                None,
            )
            .await;
        deny_body_tables(&runtime, false)?;
        let PrepareWaitForOutcome::Prepared(plan) = result? else {
            anyhow::bail!("External must prepare Wait")
        };
        assert!(
            matches!(&plan.registration.condition.wake_sources[0],WakeSource::ExternalIngress {external_trigger_id}
            if external_trigger_id.as_deref()==active.then_some(trigger.external_trigger_id.as_str()))
        );
        assert_eq!(owner.capture_published_root()?.token, token);
        assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
        drop(plan);
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        drop(owner);
    }
    Ok(())
}
#[tokio::test]
async fn external_wait_selected_plus_owned_material_rejects_before_clone_and_releases_on_drop(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let trigger = runtime
        .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    let request = SchedulingCaptureRequest {
        include_live: true,
        history_limit: 0,
        required_keys: Default::default(),
    };
    owner.prepare_owner_scheduling_request(&request).await?;
    let read = owner.capture_scheduling_read(&request)?;
    let (_, selected_count) = read.scheduling.selected_material_estimate()?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = external_wait_read(
        &read.scheduling,
        &runtime.inner.input_evidence_budget,
        selected_count,
        64 * 1024 * 1024,
    )
    .err()
    .context("selected alone cannot cover owned Wait material")?;
    assert!(matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::ResourceExhausted(_))
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let material = external_wait_read(
        &read.scheduling,
        &runtime.inner.input_evidence_budget,
        65_536,
        64 * 1024 * 1024,
    )?;
    assert_eq!(
        material.trigger_id.as_deref(),
        Some(trigger.external_trigger_id.as_str())
    );
    let weak = Arc::downgrade(&material.material);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(material);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn ordinary_timer_wait_never_adopts_raw_late_or_decodes_a_foreign_body() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let at = runtime.now();
    let late = TimerRecord {
        calendar: None,
        id: "raw-late-wait-timer".into(),
        agent_id: runtime.instance_agent_id().into(),
        created_at: at,
        duration_ms: 3_600_000,
        interval_ms: None,
        repeat: false,
        status: TimerStatus::Active,
        summary: None,
        next_fire_at: Some(at + chrono::Duration::hours(1)),
        last_fired_at: None,
        fire_count: 0,
    };
    runtime.runtime_db().timers().upsert(&late)?;
    let before:(usize,usize)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM wait_conditions),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |row|Ok((row.get(0)?,row.get(1)?)))?;
    let error = runtime
        .prepare_wait_for_outcome(
            runtime.instance_agent_id(),
            None,
            WaitForWakeKind::Timer,
            Some(late.id.clone()),
            "raw late must not gain authority".into(),
            None,
        )
        .await
        .err()
        .context("raw late Timer was adopted")?;
    assert!(error
        .downcast_ref::<crate::runtime_db::scheduling_snapshot::SchedulingSnapshotError>()
        .is_some());
    let aliases: usize = runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM scheduling_domain_records WHERE family='timer' AND record_id=?1",
        [&late.id],
        |row| row.get(0),
    )?;
    assert_eq!(aliases, 0);
    // Separate the foreign-owner refusal from the previous injected raw-live
    // frontier corruption; neither row ever acquired Scheduling membership.
    assert_eq!(
        runtime
            .runtime_db()
            .connection()?
            .execute("DELETE FROM timers WHERE timer_id=?1", [&late.id],)?,
        1
    );
    let mut foreign = late.clone();
    foreign.id = "foreign-wait-timer".into();
    foreign.agent_id = "other-owner".into();
    runtime.runtime_db().timers().upsert(&foreign)?;
    runtime.runtime_db().connection()?.execute(
        "UPDATE timers SET payload_json='{' WHERE timer_id=?1",
        [&foreign.id],
    )?;
    let error = runtime
        .prepare_wait_for_outcome(
            runtime.instance_agent_id(),
            None,
            WaitForWakeKind::Timer,
            Some(foreign.id.clone()),
            "foreign must stay forbidden".into(),
            None,
        )
        .await
        .err()
        .context("foreign Timer was read")?;
    assert_eq!(
        crate::runtime_error::describe_runtime_error(&error).code,
        "timer_agent_mismatch"
    );
    let after:(usize,usize)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM wait_conditions),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |row|Ok((row.get(0)?,row.get(1)?)))?;
    assert_eq!(after, before);
    Ok(())
}

#[tokio::test]
async fn prepared_timer_wait_cannot_commit_a_new_wait_after_its_actual_state_disappears(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let timer = runtime.schedule_timer(3_600_000, None, None).await?;
    stop_original_timer_loop(&runtime, &timer.id).await?;
    let mut owner = runtime.begin_mutation().await?;
    let PrepareWaitForOutcome::Prepared(plan) = owner
        .prepare_wait_for_outcome_with_id(
            runtime.instance_agent_id(),
            None,
            WaitForWakeKind::Timer,
            Some(timer.id.clone()),
            "same original scope needs actual State".into(),
            None,
            None,
        )
        .await?
    else {
        anyhow::bail!("Timer plan missing")
    };
    let before:(usize,usize)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM wait_conditions),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |row|Ok((row.get(0)?,row.get(1)?)))?;
    let state: String = runtime.runtime_db().connection()?.query_row(
        "SELECT payload_json FROM agent_states WHERE agent_id=?1",
        [runtime.instance_agent_id()],
        |row| row.get(0),
    )?;
    // Deliberate canonical corruption tests the actual writer gate; it grants no sparse permission.
    runtime.runtime_db().connection()?.execute(
        "DELETE FROM agent_states WHERE agent_id=?1",
        [runtime.instance_agent_id()],
    )?;
    let result = owner
        .commit_wait_transition(
            crate::ids::audit_event_id(),
            plan.command.clone(),
            plan.execution_protocol.clone(),
            plan.expected_task.clone(),
        )
        .await;
    assert!(
        result.is_err(),
        "cached selected facts cannot grant new NoState admission"
    );
    let after:(usize,usize,usize)=runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM wait_conditions),(SELECT COUNT(*) FROM input_mutation_receipts),(SELECT COUNT(*) FROM agent_states WHERE agent_id=?1)",
        [runtime.instance_agent_id()],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
    assert_eq!(after, (before.0, before.1, 0));
    let restore: AgentState = serde_json::from_str(&state)?;
    runtime
        .runtime_db()
        .agent_states()
        .upsert_for_fixture(&restore)?;
    drop((plan, owner));
    Ok(())
}
