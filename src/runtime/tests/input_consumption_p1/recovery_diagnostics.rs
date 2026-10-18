use super::*;

async fn rejected_idle_recovery_fixture(
    mutation: &str,
) -> (TempDir, RuntimeHandle, Arc<BatchProvider>, String, String) {
    let (home, host, sender, target, provider) = fixture().await;
    let first = send(&sender, &target, "one", "sentinel-private-anchor").await;
    let second = send(&sender, &target, "two", "sentinel-private-follower").await;
    let anchor = start_without_provider(&target).await;
    assert!(target
        .attach_idle_sender_inputs(&anchor, 24 * 1024)
        .await
        .unwrap());
    let mut source = target.storage().read_recent_turns(1).unwrap().remove(0);
    match mutation {
        "activation" => {
            source.idle_input_batch.as_mut().unwrap().activation_id =
                Some("sentinel-secret?token=credential".into());
        }
        "attachment" => source.input_message_ids.retain(|id| id != &second),
        _ => unreachable!(),
    }
    target
        .inner
        .runtime_db
        .connection()
        .unwrap()
        .execute(
            "UPDATE turn_records SET payload_json = ?1 WHERE turn_id = ?2",
            rusqlite::params![serde_json::to_string(&source).unwrap(), source.turn_id],
        )
        .unwrap();
    drop(target);
    drop(sender);
    drop(host);
    let runtime = RuntimeHandle::new(
        "batch-target",
        home.path().join("batch-target-state"),
        home.path().join("batch-workspace"),
        "http://127.0.0.1:7878".into(),
        provider.clone(),
        "fixture".into(),
        context_config(),
    )
    .unwrap();
    (home, runtime, provider, first, second)
}

#[tokio::test]
async fn p1_idle_recovery_rejection_predecessor_guard_classifies_each_condition_in_order() {
    use crate::domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding};
    use crate::runtime_error::IdleInputRecoveryPredicate as P;

    let (_home, runtime, _provider, _first, _second) =
        rejected_idle_recovery_fixture("activation").await;
    runtime.recover_scheduler_bootstrap_claims().await.unwrap();
    let anchor = start_without_provider(&runtime).await;
    let execution = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("batch-target")
        .unwrap()
        .unwrap();
    let attempt = execution
        .attempts
        .values()
        .find(|attempt| attempt.state == ExecutionAttemptState::Open)
        .unwrap();
    let prior = &execution.attempts[attempt.recovery_of_attempt_id.as_ref().unwrap()];
    let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
    let source = runtime
        .storage()
        .read_turn_by_id(&turn.replay.as_ref().unwrap().source_turn_id)
        .unwrap()
        .unwrap();
    let mut valid_batch = source.idle_input_batch.unwrap();
    valid_batch.activation_id = Some(prior.attempt_id.clone());
    assert!(input_consumption::validate_idle_recovery_predecessor(
        &anchor,
        attempt,
        Some(prior),
        &valid_batch
    )
    .is_ok());
    for (mutation, predicate, field) in [
        ("missing", P::PredecessorMissing, "predecessor"),
        (
            "no_link",
            P::PredecessorMissing,
            "attempt.recovery_of_attempt_id",
        ),
        ("state", P::PredecessorNotInterrupted, "predecessor.state"),
        ("agent", P::PredecessorAgentMismatch, "predecessor.agent"),
        (
            "source",
            P::PredecessorSourceMismatch,
            "predecessor.source_message",
        ),
        (
            "binding",
            P::PredecessorBindingMismatch,
            "predecessor.binding",
        ),
        ("activation", P::ActivationMismatch, "batch.activation_id"),
        (
            "precedence",
            P::PredecessorNotInterrupted,
            "predecessor.state",
        ),
    ] {
        let mut attempt = attempt.clone();
        let mut prior = prior.clone();
        let mut batch = valid_batch.clone();
        match mutation {
            "no_link" => attempt.recovery_of_attempt_id = None,
            "state" => prior.state = ExecutionAttemptState::Settled,
            "agent" => prior.agent_id = "foreign".into(),
            "source" => prior.source_message_id = None,
            "binding" => prior.binding = ExecutionBinding::Command,
            "activation" => batch.activation_id = None,
            "precedence" => {
                prior.state = ExecutionAttemptState::Settled;
                prior.agent_id = "foreign".into();
            }
            _ => {}
        }
        let prior = (!matches!(mutation, "missing" | "no_link")).then_some(&prior);
        let rejection =
            input_consumption::validate_idle_recovery_predecessor(&anchor, &attempt, prior, &batch)
                .unwrap_err();
        assert_eq!(rejection.predicate, predicate, "{mutation}");
        assert_eq!(rejection.field, field, "{mutation}");
    }
}

#[tokio::test]
async fn p1_idle_recovery_rejection_run_loop_commits_safe_failure_and_isolates_other_agent() {
    use crate::domain::execution_protocol::ExecutionAttemptState;

    for mutation in ["activation", "attachment"] {
        let (home, runtime, provider, first, second) =
            rejected_idle_recovery_fixture(mutation).await;
        let runner = tokio::spawn(runtime.clone().run());
        wait_for_audit_events(
            &runtime,
            64,
            |events| {
                events.iter().any(|event| {
                    event.kind == "queue_entry_settled"
                        && event.data["message_id"] == first
                        && event.data["reason"] == "idle_input_recovery_rejected"
                })
            },
            "rejected idle recovery should atomically settle without a provider request",
        )
        .await;
        assert!(provider.requests.lock().await.is_empty());
        let expected_code = if mutation == "activation" {
            "idle_input_recovery_activation_mismatch"
        } else {
            "idle_input_recovery_attachment_evidence_mismatch"
        };
        let state = runtime.inner.agent.lock().await.state.clone();
        let failure = state.last_runtime_failure.as_ref().unwrap();
        let artifact = failure.failure_artifact.as_ref().unwrap();
        assert_eq!(artifact.kind, expected_code);
        assert_eq!(artifact.metadata["recovery_disposition"], "rejected");
        assert_eq!(artifact.metadata["queue_status"], "aborted");
        assert_eq!(artifact.metadata["turn_status"], "aborted");
        assert_eq!(artifact.metadata["attempt_status"], "interrupted");
        assert!(!artifact.metadata["field"].is_empty());
        assert!(!serde_json::to_string(failure)
            .unwrap()
            .contains("sentinel-"));
        let events = runtime.storage().read_recent_events(64).unwrap();
        let rejection = events
            .iter()
            .find(|event| event.kind == "runtime_error")
            .unwrap();
        assert_eq!(rejection.data["code"], expected_code);
        assert_eq!(
            rejection.data["recovery_processing"]["queue_status"],
            "aborted"
        );
        assert!(!serde_json::to_string(rejection)
            .unwrap()
            .contains("sentinel-"));
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&first)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&second)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interjected
        );
        let turn = runtime.storage().read_recent_turns(1).unwrap().remove(0);
        assert_eq!(
            turn.terminal.as_ref().unwrap().kind,
            TurnTerminalKind::Aborted
        );
        assert!(turn.idle_input_batch.is_none());
        assert_eq!(turn.input_message_ids, [first.clone()]);
        let execution = runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("batch-target")
            .unwrap()
            .unwrap();
        let attempts: Vec<_> = execution
            .attempts
            .values()
            .filter(|attempt| attempt.source_message_id.as_deref() == Some(first.as_str()))
            .collect();
        assert_eq!(attempts.len(), 2);
        assert!(attempts
            .iter()
            .all(|attempt| attempt.state == ExecutionAttemptState::Interrupted));

        // An independent agent sharing the host's data root still boots and dispatches.
        let healthy = RuntimeHandle::new(
            "independent",
            home.path().join("independent-state"),
            home.path().join("batch-workspace"),
            "http://127.0.0.1:7878".into(),
            provider.clone(),
            "fixture".into(),
            context_config(),
        )
        .unwrap();
        let message = MessageEnvelope::new(
            "independent",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: Some("control".into()),
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "independent healthy request".into(),
            },
        )
        .with_admission(
            crate::types::MessageDeliverySurface::HttpControlPrompt,
            crate::types::AdmissionContext::ControlAuthenticated,
        );
        let message = healthy.enqueue(message).await.unwrap();
        let healthy_runner = tokio::spawn(healthy.clone().run());
        wait_for_audit_events(
            &healthy,
            64,
            |events| {
                events.iter().any(|event| {
                    event.kind == "queue_entry_settled"
                        && event.data["message_id"] == message.id
                        && event.data["status"] == "processed"
                })
            },
            "independent agent must remain runnable after a rejected recovery",
        )
        .await;
        assert_eq!(provider.requests.lock().await.len(), 1);
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&first)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        runner.abort();
        healthy_runner.abort();
        let _ = runner.await;
        let _ = healthy_runner.await;
        assert_eq!(
            runtime.recover_scheduler_bootstrap_claims().await.unwrap(),
            0
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&second)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interjected
        );
    }
}

#[tokio::test]
async fn p1_idle_recovery_rejection_terminal_fault_rolls_back_metadata_and_evidence() {
    for fault in PRE_COMMIT_FAULTS {
        let (_home, runtime, provider, first, second) =
            rejected_idle_recovery_fixture("activation").await;
        runtime.recover_scheduler_bootstrap_claims().await.unwrap();
        let anchor = start_without_provider(&runtime).await;
        let error = runtime
            .attach_idle_sender_inputs(&anchor, 24 * 1024)
            .await
            .unwrap_err();
        let mut terminal = runtime
            .build_turn_aborted_record("idle_input_recovery_rejected", None, 0)
            .await;
        let artifacts = runtime
            .build_runtime_failure_artifacts(&anchor, &error, &terminal)
            .await
            .unwrap();
        let mut turn_record = runtime.build_turn_record(&terminal).await.unwrap();
        turn_record
            .produced_brief_ids
            .push(artifacts.brief.id.clone());
        terminal.no_brief_reason = None;
        turn_record.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(&terminal));
        let transition = turn::TurnTerminalTransition {
            terminal,
            turn_record,
            prepared_work_item_completion: None,
            prepared_wait_for: None,
            terminal_tool_executions: Vec::new(),
        };
        let mut state = runtime.inner.agent.lock().await.state.clone();
        state.last_runtime_failure = Some(artifacts.failure_summary.clone());
        let record = QueueEntryRecord {
            message_id: anchor.id.clone(),
            agent_id: anchor.agent_id.clone(),
            priority: anchor.priority,
            status: QueueEntryStatus::Aborted,
            created_at: anchor.created_at,
            updated_at: Utc::now(),
        };
        let before_state = runtime
            .inner
            .runtime_db
            .agent_states()
            .latest("batch-target")
            .unwrap();
        let before_turns = runtime.storage().read_recent_turns(8).unwrap();
        let before_execution = runtime
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("batch-target")
            .unwrap();
        let before_events = runtime.storage().read_recent_events(64).unwrap();
        let before_queue = runtime
            .inner
            .runtime_db
            .queue_entries()
            .latest(&first)
            .unwrap();
        runtime.inject_next_transition_fault(fault);
        let result = runtime
            .commit_queue_terminal_settlement_with_evidence(
                record.clone(),
                vec![AuditEvent::legacy(
                    "idle_recovery_test_settlement",
                    serde_json::json!({"message_id": first}),
                )],
                true,
                Some(&transition),
                Some(state.clone()),
                vec![artifacts.transcript.clone()],
                vec![artifacts.brief.clone()],
            )
            .await;
        assert!(result.is_err(), "{fault:?}");
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .agent_states()
                .latest("batch-target")
                .unwrap(),
            before_state,
            "{fault:?}"
        );
        assert_eq!(
            runtime.storage().read_recent_turns(8).unwrap(),
            before_turns,
            "{fault:?}"
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .transitions()
                .load_execution_protocol_state_if_initialized("batch-target")
                .unwrap(),
            before_execution,
            "{fault:?}"
        );
        assert_eq!(
            runtime.storage().read_recent_events(64).unwrap(),
            before_events,
            "{fault:?}"
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&first)
                .unwrap(),
            before_queue,
            "{fault:?}"
        );
        assert!(runtime
            .storage()
            .read_brief_by_id(&artifacts.brief.id)
            .unwrap()
            .is_none());
        assert!(runtime
            .commit_queue_terminal_settlement_with_evidence(
                record,
                Vec::new(),
                true,
                Some(&transition),
                Some(state),
                vec![artifacts.transcript],
                vec![artifacts.brief],
            )
            .await
            .unwrap());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&first)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Aborted
        );
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .queue_entries()
                .latest(&second)
                .unwrap()
                .unwrap()
                .status,
            QueueEntryStatus::Interjected
        );
        assert!(provider.requests.lock().await.is_empty());
    }
}
