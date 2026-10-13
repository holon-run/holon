//! Post-dispatch posture must retain the original run until whole queue ACK.
use super::*;
use crate::runtime_db::dispatch_memory::{load_dispatch_memory_tx, DispatchMemoryRecord};
use crate::runtime_db::episode_memory::{EpisodeMemoryInvocation, EpisodeMemoryKey};
use crate::runtime_db::transitions::TransitionFaultPoint;
use std::{future::Future, pin::Pin};

fn memory(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
    turn: &TurnRecord,
) -> Result<DispatchMemoryRecord> {
    let key = EpisodeMemoryKey {
        agent_id: message.agent_id.clone(),
        identity_incarnation: runtime.instance_incarnation(),
        actual_dispatch_turn_id: turn.turn_id.clone(),
    };
    let invocation = EpisodeMemoryInvocation {
        message_id: message.id.clone(),
        turn_index: turn.turn_index,
    };
    runtime.runtime_db().transaction(|tx| {
        load_dispatch_memory_tx(tx, &key, &invocation)?.context("actual memory checkpoint missing")
    })
}

fn exercise_post_dispatch_window(
    model_dispatch: bool,
) -> Pin<Box<impl Future<Output = Result<()>> + Send>> {
    Box::pin(async move {
        let home = tempfile::tempdir()?;
        let workspace = tempfile::tempdir()?;
        let runtime = RuntimeHandle::new(
            "default",
            home.path().to_owned(),
            workspace.path().to_owned(),
            String::new(),
            Arc::new(crate::provider::StubProvider::new("done")),
            "default".into(),
            tests::support::context_config(),
        )?;
        let message = if model_dispatch {
            MessageEnvelope::new(
                "default",
                MessageKind::OperatorPrompt,
                MessageOrigin::Operator {
                    actor_id: None,
                    actor_display_name: None,
                },
                AuthorityClass::OperatorInstruction,
                Priority::Normal,
                MessageBody::Text {
                    text: "finish this dispatch".into(),
                },
            )
        } else {
            let mut message = MessageEnvelope::new(
                "default",
                MessageKind::TaskStatus,
                MessageOrigin::Task {
                    task_id: "original-status-task".into(),
                },
                AuthorityClass::RuntimeInstruction,
                Priority::Background,
                MessageBody::Text {
                    text: "the task is running".into(),
                },
            );
            message.task_id = Some("original-status-task".into());
            message.metadata = Some(serde_json::json!({
                "task_id": "original-status-task", "task_kind": "command_task", "task_status": "running"
            }));
            message
        };
        runtime.enqueue(message).await?;
        let scheduler_executor::RunLoopPoll::Message(scheduled) =
            scheduler_executor::SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await?
        else {
            anyhow::bail!("actual source was not claimed")
        };
        let source = scheduled.message.clone();
        let original_run = scheduled
            .running_state
            .current_run_id
            .clone()
            .context("claimed run missing")?;
        let started = std::time::Instant::now();
        let transition = runtime
            .process_message_with_plan_deferred(
                scheduled.message,
                scheduled.dispatch_plan,
                &scheduled.scheduler_decision,
            )
            .await?;
        let before_finish = runtime.agent_state().await?;
        assert_eq!(
            before_finish.current_run_id.as_deref(),
            Some(original_run.as_str())
        );
        assert_eq!(
            before_finish.current_turn_id.as_deref(),
            Some(transition.turn_record.turn_id.as_str())
        );
        assert!(matches!(
            memory(&runtime, &source, &transition.turn_record)?,
            DispatchMemoryRecord::Started { .. }
        ));
        if model_dispatch {
            assert_eq!(before_finish.status, AgentStatus::Asleep);
        }

        // This is the ordinary production Task invocation writer which used to
        // reject this exact window before the original queue could finish.
        runtime
            .create_agent_message_wait_task(
                "concurrent invocation".into(),
                "request".into(),
                "peer".into(),
                AuthorityClass::OperatorInstruction,
            )
            .await?;
        // A Task-only write has no D4 Work footprint. Its successful original
        // transaction must preserve the Started checkpoint and physical run.
        assert!(matches!(
            memory(&runtime, &source, &transition.turn_record)?,
            DispatchMemoryRecord::Started { .. }
        ));
        runtime
            .create_work_item(
                "deferred Work during original dispatch".into(),
                None,
                None,
                Vec::new(),
            )
            .await?;
        assert!(matches!(
            memory(&runtime, &source, &transition.turn_record)?,
            DispatchMemoryRecord::Pending { .. }
        ));
        assert_eq!(
            runtime.agent_state().await?.current_run_id.as_deref(),
            Some(original_run.as_str())
        );

        let entry = QueueEntryRecord {
            message_id: source.id.clone(),
            agent_id: source.agent_id.clone(),
            priority: source.priority.clone(),
            status: QueueEntryStatus::Processed,
            created_at: source.created_at,
            updated_at: runtime.now(),
        };
        runtime.inject_next_terminal_transition_fault(TransitionFaultPoint::BeforeCommit);
        let error = runtime
            .commit_queue_terminal_settlement(entry, Vec::new(), false, Some(&transition))
            .await
            .unwrap_err();
        tests::support::assert_injected_transition_fault(&error);
        assert_eq!(
            runtime.agent_state().await?.current_run_id.as_deref(),
            Some(original_run.as_str())
        );
        assert!(runtime.current_run_abort_token().await.is_some());
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&source.id)?
                .unwrap()
                .status,
            QueueEntryStatus::Dequeued
        );
        assert!(matches!(
            memory(&runtime, &source, &transition.turn_record)?,
            DispatchMemoryRecord::Pending { .. }
        ));

        runtime
            .finish_scheduled_message_processing(
                &source,
                &original_run,
                started,
                Ok(transition.clone()),
            )
            .await?;
        assert!(runtime.agent_state().await?.current_run_id.is_none());
        assert!(runtime.current_run_abort_token().await.is_none());
        assert_eq!(
            runtime
                .runtime_db()
                .queue_entries()
                .latest(&source.id)?
                .unwrap()
                .status,
            QueueEntryStatus::Processed
        );
        assert!(matches!(
            memory(&runtime, &source, &transition.turn_record)?,
            DispatchMemoryRecord::Done { .. }
        ));
        drop(scheduled.run_completion);
        Ok(())
    })
}

#[tokio::test]
async fn reducer_posture_keeps_original_run_through_concurrent_task_and_terminal_rollback(
) -> Result<()> {
    exercise_post_dispatch_window(false).await
}

#[tokio::test]
async fn sleeping_posture_keeps_original_run_through_concurrent_task_and_terminal_rollback(
) -> Result<()> {
    exercise_post_dispatch_window(true).await
}
