use super::*;
use crate::runtime::resident_task::{TaskAvailability, TaskFamily, TaskKey, TaskReadError};
use crate::runtime::task_reads;
use crate::runtime::tests::support::context_config;
use crate::runtime_db::task_result_settlement::TaskResultSettlementState;
use crate::types::WorkItemPlanStatus;

#[inline(never)]
fn actual_result() -> std::pin::Pin<
    Box<
        impl std::future::Future<
                Output = Result<(
                    tempfile::TempDir,
                    RuntimeHandle,
                    ScheduledMessage,
                    MessageEnvelope,
                )>,
            > + Send,
    >,
> {
    Box::pin(async move {
        let home = tempfile::tempdir()?;
        let runtime = RuntimeHandle::new(
            "dispatch-task-reader",
            home.path().into(),
            None::<crate::types::WorkspaceEntry>,
            String::new(),
            Arc::new(crate::provider::StubProvider::new("unused")),
            "dispatch-task-reader".into(),
            context_config(),
        )?;
        let work = runtime
            .create_work_item(
                "original Task rejoin".into(),
                Some(WorkItemPlanStatus::Ready),
                None,
                Vec::new(),
            )
            .await?;
        let mut source = MessageEnvelope::new(
            "dispatch-task-reader",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual Task producer".into(),
            },
        );
        source.work_item_id = Some(work.id.clone());
        runtime.enqueue(source).await?;
        let RunLoopPoll::Message(scheduled) =
            SchedulerDecisionExecutor::new(&runtime).poll().await?
        else {
            anyhow::bail!("actual Task fixture needs original claim")
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
        let at = runtime.now();
        let task = TaskRecord {
            id: "actual-dispatch-task".into(),
            agent_id: "dispatch-task-reader".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: Some(scheduled.message.id.clone()),
            work_item_id: Some(work.id.clone()),
            summary: Some("original Task".into()),
            detail: Some(
                runtime
                    .task_creation_detail("actual-dispatch-task", serde_json::json!({}))
                    .await?,
            ),
            recovery: None,
        };
        runtime
            .persist_task_transition(&task, "dispatch_task_reader_running")
            .await?;
        // The original permitted legacy branch has no task_kind. The canonical
        // Task/Result/Message are still written by the real whole Task transaction.
        let mut result = MessageEnvelope::new(
            "dispatch-task-reader",
            MessageKind::TaskResult,
            MessageOrigin::Task {
                task_id: task.id.clone(),
            },
            AuthorityClass::RuntimeInstruction,
            Priority::Background,
            MessageBody::Text {
                text: "actual terminal result".into(),
            },
        )
        .with_admission(
            MessageDeliverySurface::TaskRejoin,
            AdmissionContext::RuntimeOwned,
        );
        result.task_id = Some(task.id.clone());
        result.work_item_id = Some(work.id);
        result.metadata = Some(
            serde_json::json!({"task_id":task.id,"task_status":"completed","task_detail":task.detail}),
        );
        let mut terminal = task;
        terminal.status = TaskStatus::Completed;
        terminal.updated_at = runtime.now();
        terminal.parent_message_id = Some(result.id.clone());
        runtime
            .commit_terminal_task_result(&terminal, "dispatch_task_reader_terminal", &result)
            .await?;
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(
                Some(&result),
                Default::default(),
                Default::default(),
                runtime.now(),
            )
            .await?;
        let inputs = owner.capture_input_evidence()?;
        let actual = super::super::provider_recovery::captured_message(&inputs, &result.id)?
            .context("actual Result ACK lost its source")?;
        drop(owner);
        Ok((home, runtime, scheduled, actual))
    })
}

fn deny_task_bodies(runtime: &RuntimeHandle, deny: bool) -> Result<()> {
    runtime.runtime_db().transaction(|tx| {
        for table in ["tasks", "task_result_settlements", "messages"] {
            if deny {
                tx.execute(
                    &format!("ALTER TABLE {table} RENAME TO forbidden_{table}"),
                    [],
                )?;
            } else {
                tx.execute(
                    &format!("ALTER TABLE forbidden_{table} RENAME TO {table}"),
                    [],
                )?;
            }
        }
        Ok(())
    })
}

async fn finish_original(runtime: &RuntimeHandle, scheduled: ScheduledMessage) -> Result<()> {
    let terminal = runtime
        .build_reducer_only_terminal_transition("dispatch_task_reader_fixture", false)
        .await?;
    let run = scheduled
        .running_state
        .current_run_id
        .as_deref()
        .context("actual claim lost run")?;
    runtime
        .finish_scheduled_message_processing(
            &scheduled.message,
            run,
            std::time::Instant::now(),
            Ok(terminal),
        )
        .await?;
    // The original physical guard remains until the complete Queue/Memory ACK.
    drop(scheduled);
    Ok(())
}

#[tokio::test]
async fn actual_task_result_dispatch_and_recovery_decisions_do_not_read_body_tables() -> Result<()>
{
    let (_home, runtime, scheduled, result) = actual_result().await?;
    let mut owner = runtime.begin_mutation().await?;
    let read = owner
        .prepare_scheduler_read(
            Some(&result),
            Default::default(),
            Default::default(),
            runtime.now(),
        )
        .await?;
    let inputs = owner.capture_input_evidence()?;
    drop(owner);
    let metadata = read
        .tasks
        .coverage
        .result_metadata
        .values()
        .find(|row| row.message_id == result.id)
        .context("actual Result not observed")?;
    let record = match &read
        .tasks
        .lookup(&TaskKey::new(
            TaskFamily::ResultSettlement,
            &metadata.result_identity,
        ))?
        .unwrap()
        .fact
    {
        super::super::resident_task::CanonicalTaskFact::ResultSettlement(row) => row,
        _ => anyhow::bail!("actual Result has wrong family"),
    };
    let executor = SchedulerDecisionExecutor::new(&runtime);
    assert!(executor.task_result_record_is_deliverable(&result.agent_id, record)?);
    let closure =
        RuntimeHandle::closure_decision_from_projection(&read.state, &read.projection, None);
    let expected = runtime.build_message_dispatch_plan(&result, closure.clone(), &read.state)?;
    assert!(super::super::tasks::task_from_message(&result, &result.agent_id).is_err());
    let mut recovery = MessageEnvelope::new(
        &result.agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Background,
        MessageBody::Text {
            text: "retry original result".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    recovery.work_item_id = result.work_item_id.clone();
    recovery.source_refs.insert(
        "task_result_owner_key".into(),
        super::super::task_result_recovery::recovery_owner_key(record),
    );
    let scenario = scheduler::CanonicalActivationScenario::ExactTaskRejoin {
        task_id: record.task_id.clone(),
        work_item_id: result.work_item_id.clone().unwrap(),
        wait_id: None,
    };
    let work = read
        .work
        .work_item(result.work_item_id.as_deref().unwrap())?
        .unwrap();
    let generation = match inputs.execution_work_item(&work.id)? {
        super::super::resident_input_evidence::ObservedWorkExecution::Present(row) => {
            row.generation()
        }
        _ => work.revision.max(1),
    };
    let attempt = inputs.next_execution_attempt_id(&result.id)?;
    let expected_protocol = executor.plan_execution_protocol_claim_from_captured(
        &inputs,
        &read.tasks,
        &result,
        &scenario,
        &attempt,
        Some((work, generation)),
        None,
        None,
    );
    deny_task_bodies(&runtime, true)?;
    let observed_protocol = executor.plan_execution_protocol_claim_from_captured(
        &inputs,
        &read.tasks,
        &result,
        &scenario,
        &attempt,
        Some((work, generation)),
        None,
        None,
    );
    match expected_protocol {
        Ok(_) => assert!(observed_protocol.is_ok(), "{observed_protocol:?}"),
        Err(expected) => assert_eq!(
            format!("{expected:#}"),
            format!("{:#}", observed_protocol.unwrap_err())
        ),
    }
    let plan = runtime.build_message_dispatch_plan_from_work(
        &result,
        closure,
        &read.state,
        &read.work,
        &read.tasks,
    )?;
    assert!(
        plan._task_budget.is_some(),
        "Scheduled Task copy needs its own material guard"
    );
    assert_eq!(
        plan.task.as_ref().map_err(|e| anyhow::anyhow!("{e:#}"))?,
        expected
            .task
            .as_ref()
            .map_err(|e| anyhow::anyhow!("{e:#}"))?
    );
    assert!(task_reads::task_result_recovery_deliverable(
        &recovery,
        &read.tasks,
        &read.work
    )?);
    assert!(task_reads::dispatch_task_rejoin(&read.tasks, &record.task_id)?.is_some());
    // The full production activation planner consumes this same captured Task
    // after source/Work checks. Its remaining authority-fence adapter is D6/D7.
    executor.canonical_activation_plan(
        &read.projection,
        &result,
        &plan,
        false,
        &inputs,
        &read.work,
        &read.work_queue,
        &read.tasks,
    )?;
    deny_task_bodies(&runtime, false)?;
    drop((expected, plan, read, inputs));
    finish_original(&runtime, scheduled).await
}

#[tokio::test]
async fn actual_result_capture_keeps_open_admission_but_retry_does_not_select_it() -> Result<()> {
    let (_home, runtime, scheduled, result) = actual_result().await?;
    let activation = scheduled
        .dispatch_plan
        .execution_admission_provenance
        .as_ref()
        .and_then(|value| match value {
            ExecutionAdmissionProvenance::Canonical { activation_id, .. } => {
                Some(activation_id.clone())
            }
            _ => None,
        })
        .context("actual claim needs canonical activation")?;
    let at = runtime.now();
    runtime
        .apply_task_result_operation((&result.id, &activation, at), || {
            crate::runtime_db::transitions::TaskResultOperation::AdmitMessage {
                message_id: result.id.clone(),
                activation_id: activation.clone(),
                at,
            }
        })
        .await?
        .record()?;
    let mut owner = runtime.begin_mutation().await?;
    let read = owner
        .prepare_scheduler_read(
            Some(&result),
            Default::default(),
            Default::default(),
            runtime.now(),
        )
        .await?;
    drop(owner);
    let metadata = read
        .tasks
        .coverage
        .result_metadata
        .values()
        .find(|row| row.message_id == result.id)
        .context("open Result missing")?;
    assert_eq!(metadata.state, TaskResultSettlementState::CallerAdmitted);
    assert!(metadata.activation_open);
    assert!(read
        .tasks
        .coverage
        .unsettled_result_ids
        .contains(&metadata.result_identity));
    let mut recovery = MessageEnvelope::new(
        &result.agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Background,
        MessageBody::Text {
            text: "retry".into(),
        },
    )
    .with_admission(
        MessageDeliverySurface::RuntimeSystem,
        AdmissionContext::RuntimeOwned,
    );
    recovery.work_item_id = result.work_item_id.clone();
    let key = TaskKey::new(TaskFamily::ResultSettlement, &metadata.result_identity);
    let record = match &read.tasks.lookup(&key)?.unwrap().fact {
        super::super::resident_task::CanonicalTaskFact::ResultSettlement(row) => row,
        _ => unreachable!(),
    };
    recovery.source_refs.insert(
        "task_result_owner_key".into(),
        super::super::task_result_recovery::recovery_owner_key(record),
    );
    deny_task_bodies(&runtime, true)?;
    assert!(!task_reads::task_result_recovery_deliverable(
        &recovery,
        &read.tasks,
        &read.work
    )?);
    recovery.source_refs.remove("task_result_owner_key");
    recovery
        .source_refs
        .insert("task_result_message_id".into(), result.id.clone());
    recovery.source_refs.insert(
        "task_result_identity".into(),
        metadata.result_identity.clone(),
    );
    assert!(task_reads::task_result_recovery_deliverable(
        &recovery,
        &read.tasks,
        &read.work
    )?);
    deny_task_bodies(&runtime, false)?;
    drop(read);
    finish_original(&runtime, scheduled).await
}

#[tokio::test]
async fn actual_task_dispatch_rejects_pruned_foreign_and_incomplete_captures_without_fallback(
) -> Result<()> {
    let (_home, runtime, scheduled, result) = actual_result().await?;
    let mut owner = runtime.begin_mutation().await?;
    let mut read = owner
        .prepare_scheduler_read(
            Some(&result),
            Default::default(),
            Default::default(),
            runtime.now(),
        )
        .await?;
    drop(owner);
    let closure =
        RuntimeHandle::closure_decision_from_projection(&read.state, &read.projection, None);
    let task = TaskKey::new(TaskFamily::Task, result.task_id.as_ref().unwrap());
    deny_task_bodies(&runtime, true)?;
    read.tasks
        .coverage
        .observations
        .insert(task.clone(), TaskAvailability::Pruned);
    let error = runtime
        .build_message_dispatch_plan_from_work(
            &result,
            closure.clone(),
            &read.state,
            &read.work,
            &read.tasks,
        )
        .err()
        .context("Pruned Task was treated as raw fallback")?;
    assert!(
        matches!(error.downcast_ref::<TaskReadError>(),Some(TaskReadError::UnavailableQuery(key)) if key == &task)
    );
    read.tasks
        .coverage
        .observations
        .insert(task.clone(), TaskAvailability::Present);
    read.tasks.scope.agent_id = "foreign-incarnation-owner".into();
    assert!(runtime
        .build_message_dispatch_plan_from_work(
            &result,
            closure,
            &read.state,
            &read.work,
            &read.tasks
        )
        .is_err());
    read.tasks.scope = read.work.scope.clone();
    read.tasks.coverage.unsettled_complete = false;
    let recovery = MessageEnvelope::new(
        &result.agent_id,
        MessageKind::InternalFollowup,
        MessageOrigin::System {
            subsystem: "task_result_recovery".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Background,
        MessageBody::Text {
            text: "retry".into(),
        },
    );
    let error = task_reads::task_result_recovery_deliverable(&recovery, &read.tasks, &read.work)
        .unwrap_err();
    assert!(matches!(
        error.downcast_ref::<TaskReadError>(),
        Some(TaskReadError::IncompleteResults)
    ));
    deny_task_bodies(&runtime, false)?;
    drop(read);
    finish_original(&runtime, scheduled).await
}
