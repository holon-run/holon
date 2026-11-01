use super::*;
use crate::runtime::tests::support::LifecycleHarness;
use crate::types::{
    AdmissionContext, AgentMessageCallerContext, AgentMessageDeliveryOutcome,
    AgentMessagePrincipalKind, AgentMessageSendRequest, TaskKind, TaskRecoverySpec, TaskStatus,
};

fn waiting_task(runtime: &RuntimeHandle, id: &str) -> TaskRecord {
    let at = runtime.now();
    TaskRecord {
        id: id.into(),
        agent_id: "default".into(),
        kind: TaskKind::AgentMessageWait,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("accepted reply wait".into()),
        detail: Some(
            serde_json::json!({"target_agent_id":"original-sender","business_completion":false}),
        ),
        recovery: Some(TaskRecoverySpec::AgentMessageWait {
            summary: "accepted reply wait".into(),
            message: "original request".into(),
            target_agent_id: "original-sender".into(),
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_id: None,
            after_delivery_rowid: None,
            reply_deadline_at: None,
            requester_incarnation: Some(runtime.instance_incarnation()),
        }),
    }
}
fn deny_body_tables(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE tasks RENAME TO message_wait_task_body_forbidden;
            ALTER TABLE agent_states RENAME TO message_wait_state_body_forbidden;
            ALTER TABLE agent_identities RENAME TO message_wait_identity_body_forbidden;",
        )?;
        Ok(())
    })
}
fn restore_body_tables(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE message_wait_task_body_forbidden RENAME TO tasks;
            ALTER TABLE message_wait_state_body_forbidden RENAME TO agent_states;
            ALTER TABLE message_wait_identity_body_forbidden RENAME TO agent_identities;",
        )?;
        Ok(())
    })
}

async fn actual_reply(runtime: &RuntimeHandle) -> Result<MessageEnvelope> {
    let prepared = crate::runtime::agent_message_delivery::AgentMessageDeliveryService::prepare(
        AgentMessageSendRequest {
            target_agent_id: "default".into(),
            content: MessageBody::Text {
                text: "actual original reply".into(),
            },
            client_idempotency_key: "resident-wait-reply".into(),
            correlation_id: None,
            causation_id: None,
            requested_priority: None,
            forward: false,
            reply_context_id: None,
        },
        AgentMessageCallerContext {
            caller_principal: "agent:original-sender".into(),
            caller_agent_id: Some("original-sender".into()),
            principal_kind: AgentMessagePrincipalKind::PeerAgent,
            route: "agent_message".into(),
            origin: MessageOrigin::System {
                subsystem: "agent_message".into(),
            },
            authority_class: AuthorityClass::RuntimeInstruction,
            delivery_surface: MessageDeliverySurface::RuntimeSystem,
            admission_context: AdmissionContext::RuntimeOwned,
            current_turn_id: None,
            current_task_id: None,
            current_work_item_id: None,
            inherited_routing_context: None,
            routing_activation_id: None,
        },
    )?;
    let receipt = runtime
        .agent_message_delivery_service()
        .deliver(&prepared)
        .await?;
    assert_eq!(receipt.outcome, AgentMessageDeliveryOutcome::Accepted);
    let actual = runtime
        .storage()
        .read_message_by_id(&prepared.message.id)?
        .unwrap();
    assert!(actual.message_seq.is_some_and(|seq| seq > 0));
    Ok(actual)
}

#[tokio::test]
async fn accepted_message_wait_terminal_early_return_uses_current_task_without_body_sql(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = waiting_task(runtime, "message-wait-already-terminal");
    runtime
        .persist_task_transition(&original, "message_wait_original_ack")
        .await?;
    let mut terminal = original.clone();
    terminal.status = TaskStatus::Cancelled;
    terminal.updated_at += chrono::Duration::seconds(1);
    runtime
        .persist_task_transition(&terminal, "message_wait_terminal_ack")
        .await?;
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    deny_body_tables(runtime)?;
    runtime
        .finish_agent_message_wait(
            &original,
            TaskStatus::Failed,
            serde_json::json!({"unused_terminal_detail":true}),
            "unused failure".into(),
        )
        .await?;
    let selected = runtime.capture_current_task_record(&original.id)?;
    assert_eq!(selected.record().unwrap().status, TaskStatus::Cancelled);
    drop(selected);
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    restore_body_tables(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&original.id)?.unwrap(),
        terminal
    );
    Ok(())
}

#[tokio::test]
async fn actual_delivery_reply_reconciliation_keeps_complete_active_waits_without_task_body_sql(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = waiting_task(runtime, "message-wait-before-delivery-receipt");
    runtime
        .persist_task_transition(&original, "message_wait_original_ack")
        .await?;
    let actual = actual_reply(runtime).await?;
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    deny_body_tables(runtime)?;
    let mut foreign = actual.clone();
    foreign.agent_id = "other-reply-target".into();
    let error = runtime
        .reconcile_agent_message_reply(&foreign)
        .await
        .err()
        .context("foreign reply must be rejected")?;
    assert!(format!("{error:#}").contains("reply target differs from runtime scope"));
    assert!(matches!(
        runtime.reconcile_agent_message_reply(&actual).await?,
        crate::runtime::tasks::ReplyReconciliation::WaitingForReceipt
    ));
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        token
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    restore_body_tables(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&original.id)?.unwrap(),
        original
    );
    Ok(())
}

#[tokio::test]
async fn message_wait_current_generation_and_foreign_owner_are_rejected_without_body_fallback(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = waiting_task(runtime, "message-wait-original-generation");
    runtime
        .persist_task_transition(&original, "message_wait_original_ack")
        .await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    deny_body_tables(runtime)?;
    let mut foreign = original.clone();
    foreign.agent_id = "other-agent".into();
    let error = runtime
        .finish_agent_message_wait(
            &foreign,
            TaskStatus::Failed,
            serde_json::json!({}),
            "foreign completion".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
        Some(crate::runtime_db::task_snapshot::TaskSnapshotError::ForeignRecord(_))
    ));
    let mut changed = original.clone();
    changed.detail.as_mut().unwrap()["rejoin_generation"] = serde_json::json!(2);
    let error = runtime
        .finish_agent_message_wait(
            &changed,
            TaskStatus::Failed,
            serde_json::json!({}),
            "changed completion".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
        Some(crate::runtime_db::task_snapshot::TaskSnapshotError::CanonicalChanged(_))
    ));
    let mut missing = original.clone();
    missing.id = "unprepared-message-wait".into();
    let error = runtime
        .finish_agent_message_wait(
            &missing,
            TaskStatus::Failed,
            serde_json::json!({}),
            "missing completion".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<TaskReadError>(),
        Some(TaskReadError::NeedsPreparation(_))
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    restore_body_tables(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&original.id)?.unwrap(),
        original
    );
    Ok(())
}

#[tokio::test]
async fn message_wait_terminal_projection_is_budgeted_before_new_owned_detail_and_writes(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = waiting_task(runtime, "message-wait-large-terminal-detail");
    runtime
        .persist_task_transition(&original, "message_wait_original_ack")
        .await?;
    let max_records =
        usize::try_from(runtime.inner.input_evidence_budget.limits().record_limit)?.min(65536);
    let detail = serde_json::json!({"waiting_on":vec![0;max_records/8+1]});
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .finish_agent_message_wait(
            &original,
            TaskStatus::Failed,
            detail,
            "original failure".into(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<TaskReadError>(),
        Some(TaskReadError::ResourceExhausted(_))
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert_eq!(
        runtime.inner.mutation_owner.capture()?.unwrap().token,
        root.token
    );
    assert_eq!(
        runtime.runtime_db().tasks().latest(&original.id)?.unwrap(),
        original
    );
    Ok(())
}

#[tokio::test]
async fn message_wait_no_state_original_ack_never_falls_back_to_raw_active_task() -> Result<()> {
    use crate::runtime::child_task_producer_tests::register_child_monitor_for_test;
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = waiting_task(runtime, "message-wait-no-state");
    runtime
        .persist_task_transition(&original, "message_wait_original_ack")
        .await?;
    let mut child = waiting_task(runtime, "actual-no-state-child-source");
    child.kind = TaskKind::ChildAgentTask;
    child.recovery = Some(TaskRecoverySpec::ChildAgentTask {
        summary: "actual no-State original source".into(),
        prompt: "accepted".into(),
        authority_class: AuthorityClass::OperatorInstruction,
        workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
        lifecycle_disposition: crate::types::AgentLifecycleDisposition::Retain,
    });
    child.detail = Some(serde_json::json!({"workspace_mode":"inherit"}));
    runtime
        .persist_task_transition(&child, "actual_child_original_ack")
        .await?;
    let mut monitor =
        register_child_monitor_for_test(runtime, &child, None, None, None, false).await?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute("DELETE FROM agent_states WHERE agent_id='default'", [])?;
        Ok(())
    })?;
    monitor.start();
    monitor.finish().await??;
    assert!(monitor.source.is_durable_finished());
    assert!(runtime.inner.mutation_owner.capture()?.is_none());
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE tasks RENAME TO no_state_wait_task_body_forbidden")?;
        Ok(())
    })?;
    assert!(runtime
        .finish_agent_message_wait(
            &original,
            TaskStatus::Failed,
            serde_json::json!({}),
            "no State".into()
        )
        .await
        .is_err());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE no_state_wait_task_body_forbidden RENAME TO tasks")?;
        Ok(())
    })?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&original.id)?.unwrap(),
        original
    );
    Ok(())
}

/// Negative publication fixture only: preserve all actual acknowledged facts,
/// then explicitly remove/mark the Task view rather than inventing a new body.
impl RuntimeHandle {
    pub(crate) async fn replace_task_domain_for_reader_test(
        &self,
        tasks: Option<Arc<crate::runtime::resident_task::ResidentTaskDomain>>,
    ) -> Result<Option<Arc<crate::runtime::resident_task::ResidentTaskDomain>>> {
        let mut held = self.inner.mutation_owner.reserve().await?;
        let root = self
            .inner
            .mutation_owner
            .capture()?
            .context("reader negative fixture root missing")?;
        let old = root.tasks.clone();
        held.begin("reader-negative-task-readiness".into())?;
        held.committed_prepared_with_domains(
            root.state.clone(),
            root.episodes.clone(),
            root.input_evidence.clone(),
            root.work.clone(),
            tasks,
        )?;
        held.publish(root.state.clone())?;
        Ok(old)
    }
}

#[tokio::test]
async fn actual_reply_task_none_and_incomplete_active_view_do_not_hydrate_or_fallback() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let task = waiting_task(runtime, "message-wait-incomplete-active");
    runtime
        .persist_task_transition(&task, "message_wait_original_ack")
        .await?;
    let actual = actual_reply(runtime).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let domain = root.tasks.as_ref().unwrap();
    let mut coverage = domain.coverage().clone();
    coverage.active_complete = false;
    let limits = runtime.inner.input_evidence_budget.limits();
    let incomplete = Arc::new(
        crate::runtime::resident_task::ResidentTaskDomain::bootstrap_with_stops(
            domain.scope().clone(),
            coverage,
            domain.selected_install_records(),
            domain.selected_install_stop_records(),
            usize::try_from(limits.record_limit)?,
            usize::try_from(limits.history_per_agent_bytes + limits.pending_per_agent_bytes)?,
        )?
        .with_budget(runtime.inner.input_evidence_budget.clone())?,
    );
    deny_body_tables(runtime)?;
    for view in [None, Some(incomplete)] {
        runtime.replace_task_domain_for_reader_test(view).await?;
        let error = runtime
            .reconcile_agent_message_reply(&actual)
            .await
            .err()
            .context("unprepared Task view must reject the actual reply")?;
        assert!(
            matches!(
                error.downcast_ref::<TaskReadError>(),
                Some(TaskReadError::IncompleteActive)
            ),
            "{error:#}"
        );
    }
    runtime
        .replace_task_domain_for_reader_test(root.tasks.clone())
        .await?;
    assert!(matches!(
        runtime.reconcile_agent_message_reply(&actual).await?,
        crate::runtime::tasks::ReplyReconciliation::WaitingForReceipt
    ));
    restore_body_tables(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&task.id)?.unwrap(),
        task
    );
    Ok(())
}

#[tokio::test]
async fn real_running_task_status_dispatch_keeps_original_message_wait_generation_finishable(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    let original = waiting_task(runtime, "message-wait-real-running-status");
    runtime
        .persist_task_transition(&original, "message_wait_original_ack")
        .await?;
    let admitted = dispatch_original_running_status(runtime, &original).await?;
    let selected = runtime.capture_current_task_record(&original.id)?;
    assert_eq!(selected.record().unwrap().status, TaskStatus::Running);
    assert_eq!(selected.record().unwrap().recovery, original.recovery);
    let detail = selected.record().unwrap().detail.as_ref().unwrap();
    assert!(detail.get("parent_turn_id").is_none());
    assert!(admitted.turn_id.is_some());
    assert_eq!(
        detail
            .get("result_execution_turn_id")
            .and_then(serde_json::Value::as_str),
        admitted.turn_id.as_deref()
    );
    assert!(detail.get("rejoin_obligation_id").is_none());
    assert!(detail.get("rejoin_generation").is_none());
    drop(selected);
    runtime
        .finish_agent_message_wait(
            &original,
            TaskStatus::Failed,
            original.detail.clone().unwrap(),
            "actual original monitor failed".into(),
        )
        .await?;
    let selected = runtime.capture_current_task_record(&original.id)?;
    assert_eq!(selected.record().unwrap().status, TaskStatus::Failed);
    Ok(())
}

// Real original queue claim/Start/whole Finish shared by the generation fixtures.
async fn dispatch_original_running_status(
    runtime: &RuntimeHandle,
    original: &TaskRecord,
) -> Result<MessageEnvelope> {
    let mut status = MessageEnvelope::new(
        "default",
        MessageKind::TaskStatus,
        MessageOrigin::Task {
            task_id: original.id.clone(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Background,
        MessageBody::Text {
            text: "actual original monitor running".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::TaskRejoin,
        AdmissionContext::RuntimeOwned,
    );
    status.task_id = Some(original.id.clone());
    status.metadata = Some(
        serde_json::json!({"task_id":original.id,"task_created_at":original.created_at,"task_kind":original.kind,
        "task_status":"running","task_summary":original.summary,"task_recovery":original.recovery,
        "work_item_id":original.work_item_id,"task_detail":original.detail}),
    );
    let admitted = runtime.enqueue(status).await?;
    let crate::runtime::scheduler_executor::RunLoopPoll::Message(scheduled) =
        crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await?
    else {
        anyhow::bail!("actual Running status source was not claimed")
    };
    assert_eq!(scheduled.message.id, admitted.id);
    let processing = runtime.clone();
    tokio::spawn(async move {
        let scheduled = scheduled;
        let original_run = scheduled
            .running_state
            .current_run_id
            .as_deref()
            .context("actual Running source has no Run")?;
        // Keep the whole original Scheduled, including its physical guard,
        // through actual reducer Start and common Queue/Memory settlement.
        let transition = processing
            .process_message_with_plan_deferred(
                scheduled.message.clone(),
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            )
            .await?;
        processing
            .finish_scheduled_message_processing(
                &scheduled.message,
                original_run,
                std::time::Instant::now(),
                Ok(transition),
            )
            .await?;
        assert_eq!(
            processing
                .runtime_db()
                .queue_entries()
                .latest(&scheduled.message.id)?
                .unwrap()
                .status,
            crate::types::QueueEntryStatus::Processed
        );
        drop(scheduled.run_completion);
        Ok::<(), anyhow::Error>(())
    })
    .await??;
    Ok(admitted)
}

#[tokio::test]
async fn actual_message_wait_rejoin_contract_rejects_missing_and_changed_generation() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "create an original bound message wait".into(),
            },
        ))
        .await?;
    let crate::runtime::scheduler_executor::RunLoopPoll::Message(scheduled) =
        crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(runtime)
            .poll()
            .await?
    else {
        anyhow::bail!("bound creator source was not claimed")
    };
    let processing = runtime.clone();
    let original = tokio::spawn(async move {
        let original_run = scheduled
            .running_state
            .current_run_id
            .as_deref()
            .context("bound creator source has no Run")?;
        let transition = processing
            .process_message_with_plan_deferred(
                scheduled.message.clone(),
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            )
            .await?;
        let task = processing
            .create_agent_message_wait_task(
                "actual bound creator".into(),
                "original request".into(),
                "original-sender".into(),
                AuthorityClass::RuntimeInstruction,
                None,
            )
            .await?;
        assert_eq!(
            task.rejoin_fence()
                .map_err(anyhow::Error::msg)?
                .source
                .parent_turn_id(),
            Some(transition.turn_record.turn_id.as_str())
        );
        processing
            .finish_scheduled_message_processing(
                &scheduled.message,
                original_run,
                std::time::Instant::now(),
                Ok(transition),
            )
            .await?;
        drop(scheduled.run_completion);
        Ok::<_, anyhow::Error>(task)
    })
    .await??;
    dispatch_original_running_status(runtime, &original).await?;
    let _resource_refresh = runtime.hold_resource_refresh_for_test().await?;
    let selected = runtime.capture_current_task_record(&original.id)?;
    let accepted = selected.record().unwrap().clone();
    assert!(accepted
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_some());
    drop(selected);
    assert_eq!(
        original.rejoin_fence().unwrap(),
        accepted.rejoin_fence().unwrap()
    );
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    deny_body_tables(runtime)?;
    for case in [
        "missing_markers",
        "different_parent",
        "different_generation",
        "invalid_generation",
    ] {
        let mut candidate = accepted.clone();
        let detail = candidate.detail.as_mut().unwrap().as_object_mut().unwrap();
        match case {
            "missing_markers" => {
                detail.remove("rejoin_obligation_id");
                detail.remove("rejoin_generation");
            }
            "different_parent" => {
                detail.insert("parent_turn_id".into(), serde_json::json!("foreign-parent"));
            }
            "different_generation" => {
                detail.insert("rejoin_generation".into(), serde_json::json!(2));
            }
            "invalid_generation" => {
                detail.insert("rejoin_generation".into(), serde_json::json!(0));
            }
            _ => unreachable!(),
        }
        let error = runtime
            .finish_agent_message_wait(
                &candidate,
                TaskStatus::Failed,
                serde_json::json!({}),
                "foreign generation completion".into(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
                Some(crate::runtime_db::task_snapshot::TaskSnapshotError::CanonicalChanged(_))
            ),
            "{case}: {error:#}"
        );
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().token,
            token
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    restore_body_tables(runtime)?;
    assert_eq!(
        runtime.runtime_db().tasks().latest(&accepted.id)?.unwrap(),
        accepted
    );
    Ok(())
}

#[tokio::test]
async fn actual_unbound_message_wait_creator_preserves_partial_contract_through_running_and_finish(
) -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    assert_eq!(runtime.agent_state().await?.current_turn_id, None);
    let original = runtime
        .create_agent_message_wait_task(
            "actual unbound creator".into(),
            "original request".into(),
            "original-sender".into(),
            AuthorityClass::RuntimeInstruction,
            None,
        )
        .await?;
    assert_eq!(
        crate::runtime::tasks::unbound_task_rejoin_generation(&original),
        Some(1)
    );
    assert!(original.rejoin_fence().is_err());
    dispatch_original_running_status(runtime, &original).await?;
    let selected = runtime.capture_current_task_record(&original.id)?;
    let actual = selected.record().unwrap();
    assert_eq!(actual.status, TaskStatus::Running);
    assert_eq!(
        crate::runtime::tasks::unbound_task_rejoin_generation(actual),
        Some(1)
    );
    assert!(actual
        .detail
        .as_ref()
        .unwrap()
        .get("parent_turn_id")
        .is_none());
    drop(selected);
    let token = runtime.inner.mutation_owner.capture()?.unwrap().token;
    let resource_refresh = runtime.hold_resource_refresh_for_test().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    deny_body_tables(runtime)?;
    for case in [
        "missing_obligation",
        "missing_generation",
        "wrong_obligation",
        "zero_generation",
        "null_parent",
        "new_parent",
    ] {
        let mut candidate = original.clone();
        let detail = candidate.detail.as_mut().unwrap().as_object_mut().unwrap();
        match case {
            "missing_obligation" => {
                detail.remove("rejoin_obligation_id");
            }
            "missing_generation" => {
                detail.remove("rejoin_generation");
            }
            "wrong_obligation" => {
                detail.insert(
                    "rejoin_obligation_id".into(),
                    serde_json::json!("foreign-task"),
                );
            }
            "zero_generation" => {
                detail.insert("rejoin_generation".into(), serde_json::json!(0));
            }
            "null_parent" => {
                detail.insert("parent_turn_id".into(), serde_json::Value::Null);
            }
            "new_parent" => {
                detail.insert("parent_turn_id".into(), serde_json::json!("foreign-parent"));
            }
            _ => unreachable!(),
        }
        let error = runtime
            .finish_agent_message_wait(
                &candidate,
                TaskStatus::Failed,
                serde_json::json!({}),
                "foreign generation".into(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<crate::runtime_db::task_snapshot::TaskSnapshotError>(),
                Some(crate::runtime_db::task_snapshot::TaskSnapshotError::CanonicalChanged(_))
            ),
            "{case}: {error:#}"
        );
        assert_eq!(
            runtime.inner.mutation_owner.capture()?.unwrap().token,
            token
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    restore_body_tables(runtime)?;
    drop(resource_refresh);
    runtime
        .finish_agent_message_wait(
            &original,
            TaskStatus::Failed,
            original.detail.clone().unwrap(),
            "actual original monitor failure".into(),
        )
        .await?;
    let terminal = runtime.capture_current_task_record(&original.id)?;
    let terminal = terminal.record().unwrap();
    assert_eq!(terminal.status, TaskStatus::Failed);
    assert_eq!(
        crate::runtime::tasks::unbound_task_rejoin_generation(terminal),
        Some(1)
    );
    assert!(
        terminal.rejoin_fence().is_err(),
        "terminal source cannot invent rejoin authority"
    );
    let result = runtime
        .storage()
        .read_message_by_id(&format!("message:agent-message-wait:{}", original.id))?
        .unwrap();
    assert!(
        result.turn_id.is_some(),
        "Result source provenance remains present"
    );
    assert!(result.metadata.as_ref().unwrap()["task_detail"]
        .get("parent_turn_id")
        .is_none());
    Ok(())
}
