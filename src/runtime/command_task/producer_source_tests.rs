async fn settle_original_command_parent(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
    terminal: &super::super::turn::TurnTerminalTransition,
) -> Result<()> {
    crate::runtime::tests::support::finish_claimed_test_run(runtime).await;
    assert!(
        runtime
            .commit_queue_terminal_settlement(
                crate::types::QueueEntryRecord {
                    message_id: message.id.clone(),
                    agent_id: message.agent_id.clone(),
                    priority: message.priority.clone(),
                    status: crate::types::QueueEntryStatus::Processed,
                    created_at: message.created_at,
                    updated_at: chrono::Utc::now(),
                },
                Vec::new(),
                true,
                Some(terminal),
            )
            .await?
    );
    Ok(())
}

#[tokio::test]
async fn original_command_two_terminals_keep_old_source_after_parent_moves_on() -> Result<()> {
    use super::super::scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor};
    for recent_messages in [0, 1] {
        let (_home, _workspace, runtime) = test_runtime_with_context(ContextConfig {
            recent_messages,
            ..Default::default()
        });
        let prompt = |text: &str| {
            MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text { text: text.into() },
            )
        };
        runtime.enqueue(prompt("start two commands")).await?;
        let RunLoopPoll::Message(parent) = SchedulerDecisionExecutor::new(&runtime).poll().await?
        else {
            anyhow::bail!("command parent was not claimed")
        };
        runtime
            .begin_dispatch_turn(
                &parent.message,
                None,
                None,
                parent.dispatch_plan.execution_admission_provenance.clone(),
                &parent.dispatch_plan.prior_closure,
            )
            .await?;
        let mut commands = Vec::new();
        for index in 0..2 {
            let process = FakeRunningProcess::pending();
            let resolved = resolved_command(&runtime, &command_spec(false, false)).await;
            let task = runtime
                .register_command_task(
                    format!("old source command {index}"),
                    resolved,
                    running_command(process.clone(), "frozen result", ""),
                    AuthorityClass::OperatorInstruction,
                    false,
                    CapturedOutput::default(),
                )
                .await?;
            wait_for_latest_task(&runtime, &task.id, TaskStatus::Running).await;
            let lease = runtime
                .inner
                .task_handles
                .lock()
                .await
                .get(&task.id)
                .and_then(ManagedTaskHandle::control_lease)
                .context("Command lease missing")?;
            let producer = lease
                .original_command()?
                .context("Command producer missing")?;
            commands.push((task, process, lease, producer));
        }
        let terminal = crate::runtime::tests::support::terminal_transition_from_started_turn(
            &runtime,
            &parent.message,
        );
        settle_original_command_parent(&runtime, &parent.message, &terminal).await?;
        drop(parent);
        // Move the current binding and bounded Context history without
        // removing or corrupting any qualified evidence.
        runtime.enqueue(prompt("newer unrelated prompt")).await?;
        let RunLoopPoll::Message(newer) = SchedulerDecisionExecutor::new(&runtime).poll().await?
        else {
            anyhow::bail!("newer prompt was not claimed")
        };
        let message = newer.message.clone();
        let terminal = runtime
            .process_message_with_plan_deferred(
                newer.message,
                newer.dispatch_plan,
                &newer.scheduler_decision,
            )
            .await?;
        settle_original_command_parent(&runtime, &message, &terminal).await?;
        drop(newer.run_completion);
        for (task, process, lease, producer) in commands {
            *process.status.lock().await = Some(RunningProcessExitStatus::new(Some(0), None));
            lease.join().await?;
            runtime
                .continue_original_command_terminal(producer.clone())
                .await
                .context("old-source original terminal")?;
            assert!(producer.is_durable_finished());
            assert_eq!(
                runtime
                    .runtime_db()
                    .tasks()
                    .latest(&task.id)?
                    .unwrap()
                    .status,
                TaskStatus::Completed
            );
            let receipt = original_terminal_receipt(&runtime, &producer)?
                .context("original terminal receipt missing")?;
            let crate::runtime_db::input_mutation_receipts::InputMutationResult::CommandTaskTerminal {
                ack,
            } = &receipt.outcome.result
            else {
                anyhow::bail!("wrong original terminal receipt")
            };
            let crate::runtime_db::command_task_terminal::CommandTaskTerminalResult::Delivered {
                message_id,
            } = &ack.result
            else {
                anyhow::bail!("terminal result was not delivered")
            };
            let result = runtime.runtime_db().transaction(|tx| {
                crate::runtime_db::evidence::committed_message_tx(tx, &task.agent_id, message_id)
            })?;
            let mut mutation = runtime.begin_mutation().await?;
            let read = mutation
                .prepare_scheduler_read(
                    Some(&result),
                    Default::default(),
                    Default::default(),
                    chrono::Utc::now(),
                )
                .await
                .context("old-source scheduler preparation")?;
            read.inputs
                .execution_interrupted_attempts_for_source(&result.id)?;
            drop((read, mutation));
            runtime.operator_state_read_projection(10, 10).await?;
            runtime
                .continue_original_command_terminal(producer.clone())
                .await?;
            assert_eq!(
                runtime.runtime_db().connection()?.query_row(
                    "SELECT COUNT(*) FROM messages WHERE task_id=?1",
                    [&task.id],
                    |row| row.get::<_, usize>(0),
                )?,
                1
            );
        }
    }
    Ok(())
}
