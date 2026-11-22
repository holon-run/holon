use super::*;
use crate::{
    domain::execution_protocol::{ExecutionAttemptState, ExecutionBinding, WorkItemExecutionState},
    runtime_db::{
        provider_admission::ProviderAdmissionReservation,
        task_result_settlement::{TaskResultSettlementDisposition, TaskResultSettlementState},
    },
    types::QueueEntryStatus,
};

#[derive(Clone, Copy)]
pub(super) enum ParkedWaitFence {
    Cancelled,
    Expired,
    Replaced,
    SideEffect,
}

#[test]
fn exact_parked_wait_replay_rejects_invalid_waits_and_side_effects() {
    for kind in [
        ContinuationDispatch::TaskResult,
        ContinuationDispatch::TimerWait,
    ] {
        for fence in [
            ParkedWaitFence::Cancelled,
            ParkedWaitFence::Expired,
            ParkedWaitFence::Replaced,
            ParkedWaitFence::SideEffect,
        ] {
            run_continuation_dispatch_with_fence(
                kind,
                ContinuationDispatchFault::LocalCooling,
                Some(fence),
            );
        }
    }
}

pub(super) async fn invalidate_parked_wait(
    runtime: &RuntimeHandle,
    wait_id: &str,
    episode_key: &str,
    deadline: chrono::DateTime<Utc>,
    fence: ParkedWaitFence,
) {
    use crate::{
        runtime::resident_work::{WorkFamily, WorkKey},
        runtime_db::{
            transitions::{PostCommitEffects, TransitionCommit},
            work_snapshot::{prepare_work_write_tx, register_work_writes_tx},
        },
    };

    let db = runtime.runtime_db();
    let expected_payload: String = db
        .connection()
        .unwrap()
        .query_row(
            "SELECT payload_json FROM wait_conditions WHERE wait_condition_id=?1",
            [wait_id],
            |row| row.get(0),
        )
        .unwrap();
    let mut wait: crate::types::WaitConditionRecord =
        serde_json::from_str(&expected_payload).unwrap();
    match fence {
        ParkedWaitFence::Cancelled => {
            wait.status = crate::types::WaitConditionStatus::Cancelled;
            wait.cancelled_at = Some(deadline);
            wait.resolved_at = None;
        }
        ParkedWaitFence::Expired => wait.expires_at = Some(deadline),
        ParkedWaitFence::Replaced => {
            wait.id = format!("{}-replacement", wait.id);
            wait.status = crate::types::WaitConditionStatus::Active;
            wait.trigger_message_id = None;
            wait.triggered_at = None;
            wait.resolved_at = None;
        }
        ParkedWaitFence::SideEffect => {
            db.transaction(|tx| {
                assert_eq!(
                    tx.execute(
                        "UPDATE provider_recovery_episodes SET side_effect_boundary = 1
                         WHERE episode_key = ?1",
                        [episode_key],
                    )?,
                    1,
                );
                Ok(())
            })
            .unwrap();
            return;
        }
    }
    let keys = std::collections::BTreeSet::from([WorkKey::new(WorkFamily::Wait, &wait.id)]);
    let mut mutation = runtime.begin_mutation().await.unwrap();
    let request = mutation.owner_work_load_request().unwrap();
    mutation
        .prepare_owner_work_request(&request.capture)
        .await
        .unwrap();
    let final_state = mutation.state.clone();
    mutation
        .commit_transition(crate::ids::audit_event_id(), |scope| {
            db.transaction(|tx| {
                let proof = prepare_work_write_tx(tx, scope, &keys)?;
                if matches!(fence, ParkedWaitFence::Cancelled | ParkedWaitFence::Expired) {
                    // Fault-inject a revoked terminal snapshot using the original
                    // Work writer protocol, not a permitted terminal transition.
                    anyhow::ensure!(
                        tx.execute(
                            "UPDATE wait_conditions
                             SET status = ?2, cancelled_at = ?3, expires_at = ?4, payload_json = ?5,
                                 resolved_at = ?6
                             WHERE wait_condition_id = ?1 AND payload_json = ?7",
                            rusqlite::params![
                                wait.id,
                                if matches!(fence, ParkedWaitFence::Cancelled) {
                                    "cancelled"
                                } else {
                                    "resolved"
                                },
                                wait.cancelled_at
                                    .map(crate::runtime_db::migrations::timestamp),
                                wait.expires_at
                                    .map(crate::runtime_db::migrations::timestamp),
                                serde_json::to_string(&wait)?,
                                wait.resolved_at
                                    .map(crate::runtime_db::migrations::timestamp),
                                expected_payload,
                            ],
                        )? == 1,
                        "fixture's original resolved wait snapshot changed"
                    );
                } else {
                    anyhow::ensure!(
                        crate::runtime_db::repositories::upsert_wait_condition_tx(tx, &wait)?,
                        "replacement wait must be inserted"
                    );
                }
                let facts = register_work_writes_tx(
                    tx,
                    &proof,
                    &keys,
                    &Default::default(),
                    &final_state,
                    &request,
                )?;
                Ok(TransitionCommit {
                    applied: true,
                    effects: Box::new(PostCommitEffects {
                        work_delta: Some(facts.delta),
                        work_delta_requires_prepared_domain: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                })
            })
        })
        .await
        .unwrap();
}

#[test]
fn exact_task_result_construction_cooling_reselects_healthy_fallback_before_first_http() {
    run_continuation_dispatch(
        ContinuationDispatch::TaskResult,
        ContinuationDispatchFault::LocalCoolingFallback,
    );
}

async fn healthy_guard() -> (tempfile::TempDir, RuntimeHandle, Arc<ProviderTurnAdmission>) {
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), ["http://127.0.0.1:1/v1", "http://127.0.0.1:2"]);
    let runtime = open_runtime_with_clock(
        home.path(),
        config,
        Arc::new(crate::runtime::clock::TestClock::new(Utc::now())),
    );
    runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("a healthy root must schedule with an unreserved candidate");
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    assert!(admission.state.lock().unwrap().lease.is_none());
    assert_eq!(admission.episode().unwrap().total_requests, 0);
    (home, runtime, admission)
}

#[tokio::test]
async fn guard_admits_once_atomically_without_spending_a_probe_on_local_builds() {
    let (_home, runtime, admission) = healthy_guard().await;
    admission.before_dispatch().unwrap();
    {
        let state = admission.state.lock().unwrap();
        assert!(state.dispatched);
        assert!(state.dispatch.is_some());
        assert!(!state.unknown_dispatch);
        let lease = state
            .lease
            .as_ref()
            .expect("the send must reserve at admit");
        assert!(
            !lease.is_probe(),
            "a local payload build must not spend the probe budget"
        );
    }
    assert_eq!(admission.episode().unwrap().total_requests, 1);
    assert_eq!(admission.episode().unwrap().request_count, 1);

    let response = usable_guard_response();
    admission.after_request(Ok(Some(&response))).unwrap();
    let episode = admission.episode().unwrap();
    assert_eq!(episode.total_requests, 1);
    assert_eq!(episode.request_count, 1);
    assert_eq!(episode.state, ProviderRecoveryBudgetState::Resolved);
    {
        let state = admission.state.lock().unwrap();
        assert!(state.lease.is_none());
        assert!(state.dispatch.is_none());
        assert!(!state.unknown_dispatch);
    }
    assert!(runtime
        .runtime_db()
        .provider_admission_blocked(&admission.candidate.scopes, runtime.now())
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn operator_guard_probes_a_local_cooldown_and_charges_once() {
    let (_home, runtime, admission) = healthy_guard().await;
    let concurrent = concurrent_lease(&runtime, &admission);
    assert!(runtime
        .runtime_db()
        .record_provider_admission_failure(
            &concurrent,
            &admission.candidate.connection,
            ProviderCooldownPolicy::Transport,
            runtime.now(),
            None,
            &admission.candidate.model.as_string(),
        )
        .unwrap());
    admission.before_dispatch().unwrap();
    assert!(admission
        .state
        .lock()
        .unwrap()
        .lease
        .as_ref()
        .unwrap()
        .is_probe());
    assert_eq!(admission.episode().unwrap().total_requests, 1);
    assert!(admission.before_dispatch().is_err());
    assert_eq!(admission.episode().unwrap().total_requests, 1);
}

#[tokio::test]
async fn guard_rejects_a_construction_time_cooldown_without_charging_or_reserving() {
    let (_home, runtime, admission) = healthy_guard().await;
    let concurrent = concurrent_lease(&runtime, &admission);
    let db = runtime.inner.runtime_db.clone();
    let connection = admission.candidate.connection.clone();
    let model = admission.candidate.model.as_string();
    let failed_at = runtime.now();
    let pending = std::sync::Mutex::new(Some((db, concurrent, connection, model, failed_at)));
    *admission.dispatch_barrier.lock().unwrap() = Some(Box::new(move || {
        let (db, lease, connection, model, now) = pending
            .lock()
            .unwrap()
            .take()
            .expect("the barrier runs exactly once");
        assert!(db
            .record_provider_admission_failure(
                &lease,
                &connection,
                ProviderCooldownPolicy::Transport,
                now,
                Some(now + chrono::Duration::minutes(10)),
                &model,
            )
            .unwrap());
    }));
    let error = admission.before_dispatch().unwrap_err();
    assert!(matches!(
        dispatch_denial(&error).map(|denial| denial.0),
        Some(LocalDispatchDenial::ScopeCooling { .. })
    ));
    assert_eq!(admission.episode().unwrap().total_requests, 0);
    assert_eq!(admission.episode().unwrap().request_count, 0);
    {
        let state = admission.state.lock().unwrap();
        assert!(!state.dispatched);
        assert!(!state.unknown_dispatch);
        assert!(state.dispatch.is_none());
        assert!(state.lease.is_none());
    }
    assert!(runtime
        .runtime_db()
        .provider_admission_blocked(&admission.candidate.scopes, runtime.now())
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn repeated_guard_calls_keep_an_inflight_request_charged_once_and_unknown() {
    let (_home, _runtime, admission) = healthy_guard().await;
    admission.before_dispatch().unwrap();
    let charged = admission.episode().unwrap();
    assert_eq!(charged.total_requests, 1);
    assert_eq!(charged.request_count, 1);
    assert!(admission.state.lock().unwrap().dispatch.is_some());

    for _ in 0..3 {
        let error = admission.before_dispatch().unwrap_err();
        assert!(matches!(
            dispatch_denial(&error).map(|denial| denial.0),
            Some(LocalDispatchDenial::UnknownDispatch)
        ));
        let state = admission.state.lock().unwrap();
        assert!(state.unknown_dispatch);
        assert!(state.dispatched);
        assert!(state.dispatch.is_none());
        assert!(
            state.lease.is_some(),
            "do not release a request whose outcome is unknown"
        );
        drop(state);
        let episode = admission.episode().unwrap();
        assert_eq!(episode.total_requests, charged.total_requests);
        assert_eq!(episode.request_count, charged.request_count);
        assert_eq!(episode.epoch(), charged.epoch());
    }

    // A later callback cannot infer success for the discarded unknown dispatch.
    let response = usable_guard_response();
    admission.after_request(Ok(Some(&response))).unwrap();
    let episode = admission.episode().unwrap();
    assert_eq!(episode.total_requests, 1);
    assert_eq!(episode.request_count, 1);
    assert_ne!(episode.state, ProviderRecoveryBudgetState::Resolved);
    assert!(admission.state.lock().unwrap().unknown_dispatch);
}

fn usable_guard_response() -> ProviderTurnResponse {
    ProviderTurnResponse {
        material: Default::default(),
        blocks: vec![ModelBlock::Text {
            text: "usable".into(),
        }],
        stop_reason: None,
        input_tokens: 0,
        output_tokens: 0,
        cache_usage: None,
        provider_message_id: None,
        provider_request_id: None,
        request_diagnostics: None,
    }
}

pub(super) fn concurrent_lease(
    runtime: &RuntimeHandle,
    admission: &ProviderTurnAdmission,
) -> ProviderAdmissionLease {
    assert!(
        admission.state.lock().unwrap().lease.is_none(),
        "candidate selection must not pre-reserve the send lease"
    );
    let ProviderAdmissionReservation::Granted(lease) = runtime
        .runtime_db()
        .reserve_provider_admission(&admission.candidate.scopes, runtime.now(), PROBE_LEASE)
        .unwrap()
    else {
        panic!("a concurrent healthy request must obtain a legitimate same-scope reservation");
    };
    lease
}

pub(super) fn assert_construction_cooldown_without_provider_failure(runtime: &RuntimeHandle) {
    let events = runtime.storage().read_recent_events(256).unwrap();
    let rejection = events
        .iter()
        .find(|event| {
            event.kind == "provider_dispatch_rejected" && event.data["dispatch_occurred"] == false
        })
        .expect("a construction-time cooldown rejection must leave zero-send evidence");
    assert_eq!(rejection.data["dispatch_occurred"], false);
    assert_eq!(rejection.data["prior_dispatches"], 0);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "provider_attempt_failed")
            .count(),
        0,
        "the locally rejected request is not another provider HTTP failure"
    );
}

pub(super) fn assert_source_parked(
    runtime: &RuntimeHandle,
    message_id: &str,
    attempt_id: &str,
    waiter_id: &str,
    has_exact_wait: bool,
) {
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Interrupted
    );
    assert!(runtime
        .runtime_db()
        .provider_queue_parking("default", message_id)
        .unwrap()
        .is_some());
    let protocol = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert_eq!(
        protocol.attempts[attempt_id].state,
        ExecutionAttemptState::Interrupted
    );
    let work_state = &protocol.work_items[waiter_id].state;
    assert!(
        matches!(
            work_state,
            WorkItemExecutionState::Waiting { .. }
        ) || matches!(
            work_state,
            WorkItemExecutionState::Runnable {
                recovery_ref: Some(recovery_ref),
                ..
            } if recovery_ref == "interrupted"
        ) || !has_exact_wait
            && matches!(work_state, WorkItemExecutionState::Runnable { recovery_ref: None, .. }),
        "availability staleness must not pause work for repair or lose its exact wait; actual={work_state:?}",
    );
    if let Some(settlement) = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(message_id)
        .unwrap()
    {
        assert_ne!(settlement.state, TaskResultSettlementState::Settled);
        assert!(settlement.disposition.is_none());
        assert!(settlement.settled_at.is_none());
        assert!(settlement.recovery_paused_at.is_none());
    }
}

pub(super) fn assert_result_delivered(
    runtime: &RuntimeHandle,
    message_id: &str,
    waiter_id: &str,
    captured_owner_id: &str,
) {
    let settlement = runtime
        .runtime_db()
        .task_result_settlements()
        .latest_for_message(message_id)
        .unwrap()
        .unwrap();
    assert_eq!(settlement.state, TaskResultSettlementState::Settled);
    assert_eq!(
        settlement.disposition,
        Some(TaskResultSettlementDisposition::ModelDelivered)
    );
    assert!(settlement.settled_at.is_some());
    assert!(settlement.recovery_paused_at.is_none());
    assert_eq!(
        runtime
            .storage()
            .latest_task_record(&settlement.task_id)
            .unwrap()
            .unwrap()
            .work_item_id
            .as_deref(),
        Some(captured_owner_id),
        "delivery to the waiter must not migrate the task's captured owner"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(message_id)
            .unwrap()
            .unwrap()
            .status,
        QueueEntryStatus::Processed
    );
    assert!(runtime
        .runtime_db()
        .provider_queue_parking("default", message_id)
        .unwrap()
        .is_none());
    let protocol = runtime
        .runtime_db()
        .transitions()
        .load_execution_protocol_state_if_initialized("default")
        .unwrap()
        .unwrap();
    assert!(protocol.attempts.values().any(|attempt| {
        attempt.source_message_id.as_deref() == Some(message_id)
            && attempt.state == ExecutionAttemptState::Settled
            && attempt.binding
                == (ExecutionBinding::Agent {
                    agent_id: "default".into(),
                    attribution: TurnAttribution {
                        work_item_id: Some(waiter_id.to_owned()),
                        interaction_id: None,
                    },
                })
    }));
}
