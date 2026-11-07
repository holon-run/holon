use super::*;
use crate::runtime::clock::TestClock;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key, InputEvidenceReadError,
};
use crate::runtime_db::provider_admission::ProviderAdmissionReservation;

#[tokio::test]
async fn provider_admission_requires_captured_original_root_before_budget_mutation() {
    let (_home, runtime) = runtime();
    let message = runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("initial root must be admitted");
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    let attempt = admission.attempt.clone();
    let before = admission.episode().unwrap();
    drop(admission);
    let (state, mut inputs) = {
        let mut mutation = runtime.begin_mutation().await.unwrap();
        mutation
            .prepare_owner_inputs(
                std::collections::BTreeSet::from([
                    Key::new(Family::Message, &message.id),
                    Key::new(Family::Attempt, &attempt.attempt_id),
                ]),
                Default::default(),
            )
            .await
            .unwrap();
        (
            mutation.state.clone(),
            mutation.capture_input_evidence().unwrap(),
        )
    };
    assert!(
        crate::runtime::provider_recovery::captured_message(&inputs, &message.id)
            .unwrap()
            .is_some()
    );
    inputs.records.retain(|record| {
        record.locator.key.family != Family::Message || record.locator.key.id != message.id
    });
    let error = runtime
        .prepare_provider_turn_admission(&state, &message, &attempt, &inputs)
        .err()
        .expect("admission must not fall back to the persisted raw body");
    assert!(matches!(
        error.downcast_ref::<InputEvidenceReadError>(),
        Some(InputEvidenceReadError::NeedsPreparation(key))
            if key.family == Family::Message && key.id == message.id
    ));
    let after = runtime
        .runtime_db()
        .provider_recovery_episode(before.key())
        .unwrap()
        .unwrap();
    assert_eq!(after.kind, before.kind);
    assert_eq!(after.state, before.state);
    assert_eq!(after.request_count, before.request_count);
    assert_eq!(after.total_requests, before.total_requests);
    assert_eq!(after.candidate_cursor, before.candidate_cursor);
    assert_eq!(after.not_before, before.not_before);
}

#[tokio::test]
async fn pending_fallback_probe_does_not_wrap_to_failed_primary_after_restart() {
    probe_frontier(false).await;
}

#[tokio::test]
async fn pending_fallback_probe_keeps_a_recovered_primary_usable() {
    probe_frontier(true).await;
}

async fn probe_frontier(primary_recovers: bool) {
    let home = tempfile::tempdir().unwrap();
    let config = config_with_urls(home.path(), ["http://127.0.0.1:1", "http://127.0.0.1:2"]);
    let now = DateTime::from_timestamp_millis(Utc::now().timestamp_millis()).unwrap();
    let clock = Arc::new(TestClock::new(now));
    let runtime = open_runtime_with_clock(home.path(), config.clone(), clock.clone());
    let message = runtime.enqueue(operator()).await.unwrap();
    let RunLoopPoll::Message(scheduled) = SchedulerDecisionExecutor::new(&runtime)
        .poll()
        .await
        .unwrap()
    else {
        panic!("initial root must be admitted");
    };
    let admission = scheduled.dispatch_plan.provider_admission.unwrap();
    let attempt = admission.attempt.clone();
    let chain = runtime
        .provider_admission_chain(&runtime.agent_state().await.unwrap())
        .unwrap()
        .unwrap();
    let primary = &chain.candidates[0];
    let fallback = &chain.candidates[1];
    let db = runtime.runtime_db();
    let lease = admission.state.lock().unwrap().lease.take().unwrap();
    // Seed the durable dispatch/outcome boundary without running a model turn.
    let ProviderRecoveryDispatchResult::Charged(dispatch) = db
        .charge_provider_recovery_dispatch(&admission.episode().unwrap(), &lease, "primary", 1, now)
        .unwrap()
    else {
        panic!("first dispatch must be charged");
    };
    assert!(db
        .record_provider_admission_failure(
            &lease,
            &primary.request,
            ProviderCooldownPolicy::Transport,
            now,
            Some(now + Duration::seconds(30)),
            &primary.model.as_string(),
        )
        .unwrap());
    assert!(db
        .record_provider_recovery_request_outcome(
            &dispatch,
            ProviderRecoveryRequestOutcome::NetworkFailure(&primary.request),
            false,
            now,
        )
        .unwrap());
    // A competing root has made B unavailable for longer than A.
    let ProviderAdmissionReservation::Granted(other) = db
        .reserve_provider_admission(&fallback.scopes, now, PROBE_LEASE)
        .unwrap()
    else {
        panic!("independent fallback must initially be healthy");
    };
    let fallback_deadline = now + Duration::minutes(10);
    assert!(db
        .record_provider_admission_failure(
            &other,
            &fallback.request,
            ProviderCooldownPolicy::Transport,
            now,
            Some(fallback_deadline),
            &fallback.model.as_string(),
        )
        .unwrap());
    // Admission includes the existing server-deadline safety jitter.
    let fallback_deadline = db
        .provider_admission_blocked(&fallback.scopes, now)
        .unwrap()
        .unwrap()
        .not_before;
    assert!(db
        .park_provider_recovery_episode(
            &admission.episode().unwrap(),
            now + Duration::seconds(30),
            true,
            now,
        )
        .unwrap());
    let key = admission.episode_key.clone();
    drop(admission);
    drop(runtime);
    clock.advance(std::time::Duration::from_secs(60));
    let runtime = open_runtime_with_clock(home.path(), config, clock.clone());
    let db = runtime.runtime_db();
    let episode = db.provider_recovery_episode(&key).unwrap().unwrap();
    let episode = db
        .begin_provider_recovery_probe_epoch(&episode, clock.now())
        .unwrap()
        .unwrap();
    assert_eq!(episode.candidate_cursor, 1);
    assert_eq!(episode.kind, ProviderRecoveryBudgetKind::Network);
    if primary_recovers {
        let ProviderAdmissionReservation::Granted(healthy) = db
            .reserve_provider_admission(&primary.scopes, clock.now(), PROBE_LEASE)
            .unwrap()
        else {
            panic!("A's server deadline has elapsed");
        };
        assert!(
            db.record_provider_admission_success(&healthy, clock.now())
                .unwrap()
                > 0
        );
    }
    let inputs = {
        let mut mutation = runtime.begin_mutation().await.unwrap();
        mutation
            .prepare_owner_inputs(
                std::collections::BTreeSet::from([
                    Key::new(Family::Message, &message.id),
                    Key::new(Family::Attempt, &attempt.attempt_id),
                ]),
                Default::default(),
            )
            .await
            .unwrap();
        mutation.capture_input_evidence().unwrap()
    };
    let protocol = inputs.execution_state_if_initialized().unwrap().unwrap();
    let selected = runtime
        .select_provider_turn_candidate(
            &message,
            &attempt,
            &inputs,
            &protocol,
            &chain,
            &episode,
            &Default::default(),
        )
        .unwrap();
    if primary_recovers {
        let ProviderTurnGate::Granted(admission) = selected else {
            panic!("a recovered A must remain usable while B cools");
        };
        assert_eq!(admission.model(), &primary.model);
    } else {
        assert!(
            matches!(selected, ProviderTurnGate::Parked(_)),
            "an unfinished B probe must not wrap to another failed A probe"
        );
        let parked = db.provider_recovery_episode(&key).unwrap().unwrap();
        assert_eq!(parked.total_requests, 1);
        assert_eq!(parked.not_before, Some(fallback_deadline));
        clock.advance((fallback_deadline - clock.now()).to_std().unwrap());
        let due = db
            .begin_provider_recovery_probe_epoch(&parked, clock.now())
            .unwrap()
            .unwrap();
        let ProviderTurnGate::Granted(admission) = runtime
            .select_provider_turn_candidate(
                &message,
                &attempt,
                &inputs,
                &protocol,
                &chain,
                &due,
                &Default::default(),
            )
            .unwrap()
        else {
            panic!("B must be selected at its unchanged deadline");
        };
        assert_eq!(admission.model(), &fallback.model);
        assert_eq!(admission.episode().unwrap().total_requests, 1);
    }
}
