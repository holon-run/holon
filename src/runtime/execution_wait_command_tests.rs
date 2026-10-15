use super::*;
use crate::runtime::tests::support::{context_config, tempdir, StubProvider};
use crate::types::{CallbackDeliveryMode, CallbackDeliveryPayload};

fn fixture(home: &std::path::Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "execution-wait",
        home.into(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "execution-wait".into(),
        context_config(),
    )
}
fn deny_protocol(runtime: &RuntimeHandle, deny: bool) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        for table in [
            "execution_protocol_partitions",
            "execution_protocol_work_items",
            "execution_protocol_attempts",
            "execution_protocol_outcomes",
            "execution_protocol_command_results",
        ] {
            tx.execute_batch(&if deny {
                format!("ALTER TABLE {table} RENAME TO c1_wait_forbidden_{table}")
            } else {
                format!("ALTER TABLE c1_wait_forbidden_{table} RENAME TO {table}")
            })?;
        }
        Ok(())
    })
}
async fn start(
    runtime: &RuntimeHandle,
) -> Result<super::super::scheduler_executor::ScheduledMessage> {
    let poll = super::super::scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .poll()
        .await?;
    let super::super::scheduler_executor::RunLoopPoll::Message(scheduled) = poll else {
        anyhow::bail!("original queue claim missing")
    };
    runtime
        .begin_dispatch_turn(
            &scheduled.message,
            None,
            None,
            scheduled
                .dispatch_plan
                .execution_admission_provenance
                .clone(),
            &scheduled.dispatch_plan.prior_closure,
        )
        .await?;
    Ok(scheduled)
}
async fn finish(
    runtime: &RuntimeHandle,
    scheduled: super::super::scheduler_executor::ScheduledMessage,
    wait: Option<Box<PreparedWaitForSettlement>>,
) -> Result<()> {
    let mut terminal = runtime
        .build_reducer_only_terminal_transition("execution_wait_fixture", false)
        .await?;
    terminal.prepared_wait_for = wait;
    let run = scheduled
        .running_state
        .current_run_id
        .as_deref()
        .context("original run missing")?;
    runtime
        .finish_scheduled_message_processing(
            &scheduled.message,
            run,
            std::time::Instant::now(),
            Ok(terminal),
        )
        .await?;
    drop(scheduled);
    Ok(())
}

#[tokio::test]
async fn actual_started_wait_and_consumed_wait_route_without_protocol_body_sql() -> Result<()> {
    Box::pin(async {
        let home = tempdir()?;
        let runtime = fixture(home.path())?;
        let trigger = runtime
            .ensure_default_external_ingress(CallbackDeliveryMode::WakeHint)
            .await?;
        runtime
            .enqueue(MessageEnvelope::new(
                "execution-wait",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "actual original wait".into(),
                },
            ))
            .await?;
        let scheduled = start(&runtime).await?;
        let source_id = scheduled.message.id.clone();
        let mut owner = runtime.begin_mutation().await?;
        let token = owner.capture_published_root()?.token;
        deny_protocol(&runtime, true)?;
        let result = owner
            .prepare_wait_for_outcome_with_id(
                "execution-wait",
                None,
                WaitForWakeKind::External,
                Some("external source".into()),
                "original Wait".into(),
                None,
                Some("c1-original-wait".into()),
            )
            .await;
        deny_protocol(&runtime, false)?;
        let PrepareWaitForOutcome::Prepared(plan) = result? else {
            anyhow::bail!("original Wait missing")
        };
        assert_eq!(owner.capture_published_root()?.token, token);
        assert!(plan
            ._execution_read
            .as_ref()
            .unwrap()
            .open_attempt()
            .is_some());
        let frozen_protocol = serde_json::to_value(&plan.execution_protocol)?;
        assert_eq!(plan.execution_protocol.commands.len(), 1);
        let weak = Arc::downgrade(plan._execution_read.as_ref().unwrap());
        let wait_id = plan.registration.condition.id.clone();
        let plan_clone = plan.clone();
        drop(owner);
        finish(&runtime, scheduled, Some(plan)).await?;
        assert_eq!(
            serde_json::to_value(&plan_clone.execution_protocol)?,
            frozen_protocol
        );
        let queue_status = runtime
            .runtime_db()
            .queue_entries()
            .latest(&source_id)?
            .context("original Queue missing")?
            .status;
        assert_eq!(queue_status, QueueEntryStatus::Processed);
        assert!(
            weak.upgrade().is_some(),
            "clone keeps original selected material after ACK"
        );
        drop(plan_clone);
        assert!(weak.upgrade().is_none());
        runtime
            .deliver_callback(
                &trigger.external_trigger_id,
                CallbackDeliveryPayload {
                    body: Some(MessageBody::Text {
                        text: "real external wake".into(),
                    }),
                    content_type: Some("text/plain".into()),
                    correlation_id: None,
                    causation_id: None,
                },
            )
            .await?;
        let resumed = start(&runtime).await?;
        let condition = runtime
            .storage()
            .latest_wait_conditions()?
            .into_iter()
            .find(|wait| wait.id == wait_id)
            .context("original Wait not found after actual resume")?;
        assert_eq!(condition.status, WaitConditionStatus::Resolved);
        deny_protocol(&runtime, true)?;
        let consumed = runtime
            .canonical_consumed_wait_condition(
                "execution-wait",
                &resumed.message,
                std::slice::from_ref(&condition),
            )
            .await;
        deny_protocol(&runtime, false)?;
        assert_eq!(
            consumed?.as_ref().map(|wait| wait.id.as_str()),
            Some(wait_id.as_str())
        );
        finish(&runtime, resumed, None).await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn selected_wait_clone_keeps_guards_after_owner_release() -> Result<()> {
    let home = tempdir()?;
    let runtime = fixture(home.path())?;
    let mut owner = runtime.begin_mutation().await?;
    owner
        .prepare_owner_scheduling_request(
            &crate::runtime::resident_scheduling::SchedulingCaptureRequest {
                include_live: true,
                history_limit: 0,
                required_keys: Default::default(),
            },
        )
        .await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let PrepareWaitForOutcome::Prepared(plan) = owner
        .prepare_wait_for_outcome_with_id(
            "execution-wait",
            None,
            WaitForWakeKind::External,
            Some("external".into()),
            "held original".into(),
            None,
            None,
        )
        .await?
    else {
        anyhow::bail!("Wait plan missing")
    };
    let weak = Arc::downgrade(plan._execution_read.as_ref().unwrap());
    let clone = plan.clone();
    drop(plan);
    drop(owner);
    assert!(weak.upgrade().is_some());
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(clone);
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
