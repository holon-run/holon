use super::*;
use crate::domain::execution_protocol::{ExecutionAttemptState, WorkItemExecutionState};
use crate::runtime_error::{describe_runtime_error, RuntimeErrorDomain};
use std::sync::atomic::Ordering;

#[test]
fn local_dispatch_denial_survives_prior_http_timeline() {
    let error = crate::provider::provider_turn_error(
        "prior request failed, then local admission paused",
        crate::provider::ProviderAttemptTimeline {
            attempts: Vec::new(),
            requested_model_ref: "test".into(),
            active_model_ref: None,
            winning_model_ref: None,
            pending_fallback_model_ref: None,
            pending_fallback_disposition: None,
            aggregated_token_usage: None,
        },
        anyhow::Error::new(DispatchDeferred(LocalDispatchDenial::ScopeCooling {
            not_before: Utc::now(),
        }))
        .context("send rejected"),
    )
    .context("outer request context");
    assert!(matches!(
        error
            .chain()
            .find_map(|source| source.downcast_ref::<DispatchDeferred>())
            .map(|error| error.0),
        Some(LocalDispatchDenial::ScopeCooling { .. })
    ));
}

#[test]
fn local_dispatch_rejection_stops_without_fallback_or_autonomous_replay() {
    run_local_dispatch_scenario(0);
}

#[test]
fn cooling_dispatch_rejection_resumes_at_the_original_deadline_after_restart() {
    run_local_dispatch_scenario(1);
}

#[test]
fn dispatch_guard_storage_error_keeps_its_domain_and_retryability() {
    run_local_dispatch_scenario(2);
}

#[test]
fn parked_source_readback_reconciles_unapplied_terminal_projection() {
    run_local_dispatch_scenario(3);
}

#[test]
fn parked_source_readback_preserves_newer_durable_stop() {
    run_local_dispatch_scenario(4);
}

fn run_local_dispatch_scenario(scenario: u8) {
    let thread = std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    match scenario {
                        0 => local_dispatch_rejection_scenario().await,
                        1 => cooling_dispatch_rejection_scenario(None).await,
                        2 => storage_dispatch_rejection_scenario().await,
                        3 => cooling_dispatch_rejection_scenario(Some(false)).await,
                        4 => cooling_dispatch_rejection_scenario(Some(true)).await,
                        _ => unreachable!(),
                    }
                });
        })
        .unwrap();
    if let Err(panic) = thread.join() {
        std::panic::resume_unwind(panic);
    }
}

async fn cooling_dispatch_rejection_scenario(readback_stop: Option<bool>) {
    let server = OutageServer::start(false).await;
    server.healthy.store(true, Ordering::SeqCst);
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&server.url, &server.url]);
    let now = chrono::DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let clock = Arc::new(crate::runtime::clock::TestClock::new(now));
    let runtime = open_runtime_with_clock(home.path(), config.clone(), clock.clone());
    let work = runtime
        .create_work_item(
            "retain cooling source".into(),
            Some(crate::types::WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut input = operator();
    input.work_item_id = Some(work.id.clone());
    runtime.enqueue(input.clone()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("healthy input must be admitted");
    };
    let admission = scheduled.dispatch_plan.provider_admission.as_ref().unwrap();
    let episode_key = admission.episode_key.clone();
    let attempt_id = admission.attempt.attempt_id.clone();
    let lease = admission.state.lock().unwrap().lease.take().unwrap();
    let deadline = runtime.now() + Duration::minutes(10);
    // Another request cools the scope after admission but before this send.
    assert!(runtime
        .runtime_db()
        .record_provider_admission_failure(
            &lease,
            &admission.candidate.connection,
            ProviderCooldownPolicy::Transport,
            runtime.now(),
            Some(deadline),
            &admission.candidate.model.as_string(),
        )
        .unwrap());
    let policy_deadline = runtime
        .runtime_db()
        .provider_admission_blocked(&admission.candidate.scopes, runtime.now())
        .unwrap()
        .unwrap()
        .not_before;
    // Operator probes bypass local padding, not the original server deadline.
    assert!(policy_deadline > deadline);
    let started = std::time::Instant::now();
    let result = runtime
        .process_message_with_plan_deferred(
            scheduled.message.clone(),
            scheduled.dispatch_plan,
            &scheduled.scheduler_decision,
        )
        .await;
    assert!(is_parked_dispatch_rejection(
        result
            .as_ref()
            .err()
            .expect("cooling must reject before HTTP")
    ));
    let unapplied_state = runtime.inner.agent.lock().await.state.clone();
    runtime
        .settle_processed_message(scheduled.message, result, started)
        .await
        .unwrap();
    let parking = runtime
        .runtime_db()
        .provider_queue_parking("default", &input.id)
        .unwrap()
        .unwrap();
    assert_eq!(parking.not_before, deadline);
    assert_eq!(parking.episode_key, episode_key);
    if let Some(stop) = readback_stop {
        let record = runtime
            .runtime_db()
            .queue_entries()
            .latest(&input.id)
            .unwrap()
            .unwrap();
        let mut durable = runtime.agent_state().await.unwrap();
        let terminal = durable.last_turn_terminal.clone().unwrap();
        let transition = crate::runtime::turn::TurnTerminalTransition {
            turn_record: runtime
                .storage()
                .read_turn_by_id(&terminal.turn_id)
                .unwrap()
                .unwrap(),
            terminal,
            prepared_work_item_completion: None,
            prepared_wait_for: None,
            terminal_tool_executions: Vec::new(),
        };
        assert_eq!(
            runtime
                .terminal_settlement_readback(&record, &transition, None)
                .unwrap(),
            crate::runtime::TerminalSettlementReadback::Committed
        );
        assert!(durable.current_run_id.is_none());
        assert!(durable.last_runtime_failure.is_some());
        assert_ne!(durable.status, crate::types::AgentStatus::AwakeRunning);
        assert_eq!(
            unapplied_state.status,
            crate::types::AgentStatus::AwakeRunning
        );
        if stop {
            durable.status = crate::types::AgentStatus::Stopped;
            runtime.storage().write_agent(&durable).unwrap();
        }
        {
            // The writer committed, but its in-memory effects never ran.
            let mut guard = runtime.inner.agent.lock().await;
            guard.state = unapplied_state.clone();
            guard.last_persisted_state = unapplied_state;
            assert!(guard
                .queue
                .pop_next_matching(|queued| queued.id == input.id)
                .is_some());
        }
        for _ in 0..2 {
            runtime
                .restore_parked_provider_source(&input.id)
                .await
                .unwrap();
            let guard = runtime.inner.agent.lock().await;
            assert_eq!(guard.queue.len(), 1);
            assert_eq!(guard.state, durable);
            assert_eq!(guard.last_persisted_state, durable);
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest("default")
                    .unwrap(),
                Some(durable.clone())
            );
        }
        assert_eq!(
            runtime
                .runtime_db()
                .provider_queue_parking("default", &input.id)
                .unwrap(),
            Some(parking)
        );
        assert_eq!(server.calls.load(Ordering::SeqCst), 0);
        return;
    }
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(&input.id)
            .unwrap()
            .unwrap()
            .status,
        crate::types::QueueEntryStatus::Interrupted
    );
    for _ in 0..3 {
        clock.advance(std::time::Duration::from_secs(1));
        assert!(!matches!(
            SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .unwrap(),
            RunLoopPoll::Message(_)
        ));
    }
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    drop(runtime);
    let restored = open_runtime_with_clock(home.path(), config, clock.clone());
    assert_eq!(
        restored
            .runtime_db()
            .provider_queue_parking("default", &input.id)
            .unwrap(),
        Some(parking)
    );
    clock.advance(
        (deadline - restored.now() + Duration::seconds(1))
            .to_std()
            .unwrap(),
    );
    let RunLoopPoll::Message(resumed) = SchedulerDecisionExecutor::new(&restored)
        .poll()
        .await
        .unwrap()
    else {
        panic!("retained source must resume after its original deadline");
    };
    let admission = resumed.dispatch_plan.provider_admission.as_ref().unwrap();
    assert_eq!(admission.episode_key, episode_key);
    assert_eq!(
        admission.attempt.recovery_of_attempt_id.as_deref(),
        Some(attempt_id.as_str())
    );
    assert_eq!(admission.episode().unwrap().total_requests, 0);
    assert_eq!(admission.episode().unwrap().candidate_cursor, 0);
    let result = restored
        .process_message_with_plan_deferred(
            resumed.message,
            resumed.dispatch_plan,
            &resumed.scheduler_decision,
        )
        .await;
    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(server.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        restored
            .runtime_db()
            .provider_recovery_episode(&episode_key)
            .unwrap()
            .unwrap()
            .total_requests,
        1
    );
}

async fn storage_dispatch_rejection_scenario() {
    #[derive(Debug)]
    struct BusyGuard;
    impl ProviderDispatchGuard for BusyGuard {
        fn before_dispatch(&self) -> anyhow::Result<()> {
            Err(crate::runtime_db::RuntimeDbRetryableError::new(
                "before_dispatch",
                std::path::Path::new("fixture.sqlite"),
                "busy",
            )
            .into())
        }
    }
    let server = OutageServer::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&server.url, &server.url]);
    let provider = build_provider_from_config(&config).unwrap();
    let runtime = open_runtime(home.path(), config);
    let mut request = ProviderTurnRequest::plain(
        "storage failure",
        vec![crate::provider::ConversationMessage::UserText(
            "hello".into(),
        )],
        Vec::new(),
    );
    request.dispatch_guard = Some(Arc::new(BusyGuard));
    let error = provider.complete_turn(request).await.unwrap_err();
    assert!(crate::provider::is_dispatch_guard_rejected(&error));
    let error = runtime.local_provider_dispatch_error(error);
    let descriptor = describe_runtime_error(&error);
    assert_eq!(descriptor.domain, RuntimeErrorDomain::Storage);
    assert_eq!(descriptor.code, "runtime_db_busy");
    assert!(descriptor.retryable);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    assert!(!runtime
        .storage()
        .read_recent_events(64)
        .unwrap()
        .iter()
        .any(|event| {
            matches!(
                event.kind.as_str(),
                "provider_attempt_failed" | "provider_fallback"
            )
        }));
}

async fn local_dispatch_rejection_scenario() {
    let server = OutageServer::start(false).await;
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), [&server.url, &server.url]);
    let clock = Arc::new(crate::runtime::clock::TestClock::new(Utc::now()));
    let runtime = open_runtime_with_clock(home.path(), config.clone(), clock.clone());
    let chain = runtime
        .provider_admission_chain(&runtime.agent_state().await.unwrap())
        .unwrap()
        .unwrap();
    let past = runtime.now() - Duration::days(1);
    let crate::runtime_db::provider_admission::ProviderAdmissionReservation::Granted(lease) =
        runtime
            .inner
            .runtime_db
            .reserve_provider_admission(&chain.candidates[0].scopes, past, PROBE_LEASE)
            .unwrap()
    else {
        panic!("initial healthy lease");
    };
    runtime
        .inner
        .runtime_db
        .record_provider_admission_failure(
            &lease,
            &chain.candidates[0].connection,
            ProviderCooldownPolicy::Transport,
            past,
            None,
            &chain.candidates[0].model.as_string(),
        )
        .unwrap();
    let work = runtime
        .create_work_item(
            "retain rejected source".into(),
            Some(crate::types::WorkItemPlanStatus::Ready),
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    let mut input = operator();
    input.work_item_id = Some(work.id.clone());
    runtime.enqueue(input.clone()).await.unwrap();
    let RunLoopPoll::Message(mut scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("operator input must be admitted");
    };
    let admission = scheduled.dispatch_plan.provider_admission.as_mut().unwrap();
    assert!(admission
        .state
        .lock()
        .unwrap()
        .lease
        .as_ref()
        .unwrap()
        .is_probe());
    // Corrupt only the pre-send comparison snapshot, not canonical authority.
    // This models a post-claim stale guard while the execution still owns its lane.
    Arc::get_mut(admission)
        .expect("scheduled guard has one owner")
        .attempt
        .admitted_fences
        .host_registry_revision += 1;
    let episode_key = admission.episode_key.clone();
    let attempt_id = admission.attempt.attempt_id.clone();
    let processing_started = std::time::Instant::now();
    let result = runtime
        .process_message_with_plan_deferred(
            scheduled.message.clone(),
            scheduled.dispatch_plan,
            &scheduled.scheduler_decision,
        )
        .await;
    let error = result.as_ref().err().expect("guard must reject the send");
    let failure = describe_runtime_error(error);
    assert_eq!(failure.domain, RuntimeErrorDomain::Conflict);
    assert_eq!(failure.code, "provider_dispatch_rejected");
    assert!(!failure.retryable);
    runtime
        .settle_processed_message(scheduled.message, result, processing_started)
        .await
        .unwrap();

    let protocol = runtime
        .inner
        .runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert!(matches!(
        &protocol.work_items[&work.id].state,
        WorkItemExecutionState::NeedsRepair { .. }
    ));
    assert_ne!(
        protocol.attempts[&attempt_id].state,
        ExecutionAttemptState::Open
    );
    let episode = runtime
        .inner
        .runtime_db
        .provider_recovery_episode(&episode_key)
        .unwrap()
        .unwrap();
    assert_eq!(episode.total_requests, 0);
    assert_eq!(episode.candidate_cursor, 0);
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
    let crate::runtime_db::provider_admission::ProviderAdmissionReservation::Granted(released) =
        runtime
            .inner
            .runtime_db
            .reserve_provider_admission(&chain.candidates[0].scopes, runtime.now(), PROBE_LEASE)
            .unwrap()
    else {
        panic!("undispatched probe lease must have been released");
    };
    runtime
        .inner
        .runtime_db
        .release_provider_admission(&released)
        .unwrap();
    let turns = runtime.storage().read_recent_turns(20).unwrap();
    assert_eq!(turns.len(), 1);
    let events = runtime.storage().read_recent_events(256).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "provider_dispatch_rejected" && event.data["dispatch_occurred"] == false
    }));
    assert!(!events.iter().any(|event| {
        matches!(
            event.kind.as_str(),
            "provider_attempt_failed" | "provider_fallback" | "provider_recovery_enqueued"
        )
    }));
    for _ in 0..5 {
        clock.advance(std::time::Duration::from_secs(2));
        assert!(!matches!(
            SchedulerDecisionExecutor::new(&runtime)
                .poll()
                .await
                .unwrap(),
            RunLoopPoll::Message(_)
        ));
    }
    assert_eq!(runtime.storage().read_recent_turns(20).unwrap().len(), 1);
    assert!(runtime
        .storage()
        .read_message_by_id(&input.id)
        .unwrap()
        .is_some());
    drop(runtime);
    let restored = open_runtime_with_clock(home.path(), config, clock);
    assert!(matches!(
        restored
            .inner
            .runtime_db
            .transitions()
            .load_execution_protocol_state_if_initialized("default")
            .unwrap()
            .unwrap()
            .work_items[&work.id]
            .state,
        WorkItemExecutionState::NeedsRepair { .. }
    ));
    assert!(!matches!(
        SchedulerDecisionExecutor::new(&restored)
            .poll()
            .await
            .unwrap(),
        RunLoopPoll::Message(_)
    ));
    assert_eq!(server.calls.load(Ordering::SeqCst), 0);
}
