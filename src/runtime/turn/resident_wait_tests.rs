use super::*;
use crate::{
    runtime::{
        resident_work::{WorkCaptureRequest, WorkReadError},
        scheduler_executor::{RunLoopPoll, SchedulerDecisionExecutor},
        tests::support::{context_config, tempdir, StubProvider},
    },
    runtime_db::transitions::{ExecutionProtocolTransition, WaitTransitionCommand},
    types::{
        AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, WaitConditionKind,
        WaitConditionRecord, WaitConditionStatus, WakeSource,
    },
};
use std::sync::Arc;

#[tokio::test]
async fn terminal_wait_ids_use_prepared_all_status_relation_without_wait_sql() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new(
        "terminal-waits",
        home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        String::new(),
        Arc::new(StubProvider::new("unused")),
        "terminal-waits".into(),
        context_config(),
    )?;
    let source = runtime
        .enqueue(MessageEnvelope::new(
            "terminal-waits",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "actual started Turn".into(),
            },
        ))
        .await?;
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime).poll().await?
    else {
        anyhow::bail!("original source was not claimed");
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
    let state = runtime.agent_state().await?;
    let turn_id = state.current_turn_id.clone().unwrap();
    let now = chrono::DateTime::from_timestamp_millis(runtime.now().timestamp_millis()).unwrap();
    let original = WaitConditionRecord {
        id: "a-terminal-wait".into(),
        agent_id: state.id.clone(),
        work_item_id: None,
        status: WaitConditionStatus::Active,
        kind: WaitConditionKind::External,
        source: Some("WaitFor".into()),
        subject_ref: None,
        waiting_for: "original dependency".into(),
        wake_sources: vec![WakeSource::ExternalIngress {
            external_trigger_id: None,
        }],
        continuation: None,
        created_at: now,
        updated_at: now,
        expires_at: None,
        resolved_at: None,
        cancelled_at: None,
        turn_id: Some(turn_id.clone()),
        trigger_message_id: None,
        triggered_at: None,
    };
    let mut cancelled = original.clone();
    cancelled.status = WaitConditionStatus::Cancelled;
    cancelled.updated_at = now + chrono::Duration::seconds(1);
    cancelled.cancelled_at = Some(cancelled.updated_at);
    let mut active = original.clone();
    active.id = "b-terminal-wait".into();
    active.created_at = now + chrono::Duration::seconds(2);
    active.updated_at = active.created_at;
    for (wait, expected) in [
        (original.clone(), Vec::new()),
        (cancelled, vec![original]),
        (active, Vec::new()),
    ] {
        let mut mutation = runtime.begin_mutation().await?;
        mutation
            .commit_wait_transition(
                crate::ids::audit_event_id(),
                WaitTransitionCommand {
                    agent_id: state.id.clone(),
                    work_items: Vec::new(),
                    expected_wait_conditions: expected
                        .into_iter()
                        .map(
                            |wait| crate::runtime_db::transitions::WaitConditionExpectation {
                                id: wait.id,
                                agent_id: wait.agent_id,
                                status: wait.status,
                                updated_at: wait.updated_at,
                            },
                        )
                        .collect(),
                    wait_conditions: vec![wait],
                    timer_wake: None,
                    task_result_admission: None,
                    agent_state: None,
                    audit_events: Vec::new(),
                    index_changes: Vec::new(),
                    notify_scheduler: false,
                    fault: None,
                },
                ExecutionProtocolTransition::default(),
                None,
            )
            .await?;
    }
    // This is the explicit StartedTurn actor contract. The ordinary assembler
    // below has no permission to prepare another relation on a miss.
    let mut request = WorkCaptureRequest::current();
    request.include_live = false;
    request.nonopen_limit = 0;
    request.required_turn_ids.insert(turn_id.clone());
    let mut mutation = runtime.begin_mutation().await?;
    mutation.prepare_owner_work_request(&request).await?;
    drop(mutation);
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("ALTER TABLE wait_conditions RENAME TO hidden_wait_conditions")?;
        Ok(())
    })?;
    let terminal = TurnTerminalRecord {
        turn_id: turn_id.clone(),
        turn_index: state.turn_index,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: None,
        no_brief_reason: None,
        checkpoint: None,
        completed_at: runtime.now(),
        duration_ms: 1,
    };
    let record = runtime.build_turn_record(&terminal).await?;
    assert_eq!(record.input_message_ids, vec![source.id]);
    assert_eq!(
        record.waiting_condition_ids,
        vec!["a-terminal-wait", "b-terminal-wait"]
    );
    let missing = TurnTerminalRecord {
        turn_id: "unprepared-turn".into(),
        ..terminal
    };
    let error = runtime.build_turn_record(&missing).await.unwrap_err();
    assert!(matches!(error.downcast_ref::<WorkReadError>(),
        Some(WorkReadError::TurnWaitsNotPrepared(id)) if id == "unprepared-turn"));
    drop(scheduled);
    Ok(())
}
