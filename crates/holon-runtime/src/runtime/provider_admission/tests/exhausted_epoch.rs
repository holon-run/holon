use super::*;
use crate::runtime::clock::TestClock;
use crate::runtime::resident_input_evidence::{
    InputEvidenceFamily as Family, InputEvidenceKey as Key,
};
use crate::runtime_db::provider_admission::ProviderRecoveryDispatchResult;

#[tokio::test]
async fn settled_failed_epoch_parks_after_all_scope_deadlines_expire() {
    exhausted_epoch(ExhaustedEpochCase::Settled).await;
}

#[tokio::test]
async fn settled_failed_epoch_parks_before_recovery_attempt_is_claimed() {
    exhausted_epoch(ExhaustedEpochCase::Planned).await;
}

#[tokio::test]
async fn unknown_dispatch_without_deadline_does_not_invent_a_park() {
    exhausted_epoch(ExhaustedEpochCase::Unknown).await;
}

#[tokio::test]
async fn zero_dispatch_exclusions_do_not_invent_a_failed_epoch() {
    exhausted_epoch(ExhaustedEpochCase::ZeroSend).await;
}

#[tokio::test]
async fn excluded_candidates_do_not_prove_failed_epoch_exhaustion() {
    exhausted_epoch(ExhaustedEpochCase::Excluded).await;
}

#[derive(Clone, Copy)]
enum ExhaustedEpochCase {
    Settled,
    Planned,
    Unknown,
    ZeroSend,
    Excluded,
}

async fn exhausted_epoch(case: ExhaustedEpochCase) {
    let sent = !matches!(case, ExhaustedEpochCase::ZeroSend);
    let settled = matches!(
        case,
        ExhaustedEpochCase::Settled | ExhaustedEpochCase::Planned | ExhaustedEpochCase::Excluded
    );
    let home = tempfile::tempdir().unwrap();
    let mut config = config_with_urls(home.path(), ["http://127.0.0.1:1"; 2]);
    config.fallback_models.clear();
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
    let candidate = &chain.candidates[0];
    let db = runtime.runtime_db();
    if sent {
        let lease = stale_admission::concurrent_lease(&runtime, &admission);
        let ProviderRecoveryDispatchResult::Charged(dispatch) = db
            .charge_provider_recovery_dispatch(
                &admission.episode().unwrap(),
                &lease,
                "failed",
                1,
                now,
            )
            .unwrap()
        else {
            panic!("dispatch must be charged");
        };
        if settled {
            assert!(db
                .record_provider_admission_failure(
                    &lease,
                    &candidate.rate_limit,
                    ProviderCooldownPolicy::TransientQuota,
                    now,
                    None,
                    &candidate.model.as_string(),
                )
                .unwrap());
            assert!(db
                .record_provider_recovery_request_outcome(
                    &dispatch,
                    ProviderRecoveryRequestOutcome::RateLimited(&candidate.rate_limit),
                    false,
                    now,
                )
                .unwrap());
            let deadline = db
                .provider_admission_blocked(&candidate.scopes, now)
                .unwrap()
                .unwrap()
                .not_before;
            clock.advance((deadline - now).to_std().unwrap());
        } else {
            db.release_provider_admission(&lease).unwrap();
        }
    }
    assert!(db
        .provider_admission_blocked(&candidate.scopes, clock.now())
        .unwrap()
        .is_none());
    let episode = admission.episode().unwrap();
    assert_eq!(episode.state, ProviderRecoveryBudgetState::Ready);
    assert_eq!(episode.request_count, u32::from(sent));
    assert_eq!(episode.not_before, None);
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
    let mut selection_attempt = attempt.clone();
    if matches!(case, ExhaustedEpochCase::Planned) {
        selection_attempt.attempt_id = "planned-recovery-before-claim".into();
        selection_attempt.recovery_of_attempt_id = Some(attempt.attempt_id.clone());
        assert!(!protocol
            .attempts
            .contains_key(&selection_attempt.attempt_id));
    }
    let excluded = if matches!(
        case,
        ExhaustedEpochCase::ZeroSend | ExhaustedEpochCase::Excluded
    ) {
        std::collections::BTreeSet::from([candidate.model.as_string()])
    } else {
        Default::default()
    };
    let selected = runtime.select_provider_turn_candidate(
        &message,
        &selection_attempt,
        &inputs,
        &protocol,
        &chain,
        &episode,
        &excluded,
    );
    if !matches!(
        case,
        ExhaustedEpochCase::Settled | ExhaustedEpochCase::Planned
    ) {
        let error = selected.err().expect("ineligible epoch must fail closed");
        assert!(matches!(
            dispatch_denial(&error),
            Some(DispatchDeferred(
                LocalDispatchDenial::NoEligibleCandidate,
                None
            ))
        ));
        assert!(db
            .provider_queue_parking("default", &message.id)
            .unwrap()
            .is_none());
        return;
    }
    assert!(
        matches!(selected, Ok(ProviderTurnGate::Parked(Some(_)))),
        "a settled failed epoch must commit parking after scope cooldown expires"
    );
    let parking = db
        .provider_queue_parking("default", &message.id)
        .unwrap()
        .unwrap();
    assert!(parking.not_before > clock.now());
    let parked = db
        .provider_recovery_episode(episode.key())
        .unwrap()
        .unwrap();
    assert_eq!(parked.request_count, 1);
    assert_eq!(parked.total_requests, 1);
    assert_eq!(parked.not_before, Some(parking.not_before));
    assert!(!db
        .park_provider_recovery_episode(&parked, clock.now(), false, clock.now())
        .unwrap());
    assert!(db
        .begin_provider_recovery_probe_epoch(&parked, clock.now())
        .unwrap()
        .is_none());
    drop(runtime);
    let reopened = open_runtime_with_clock(home.path(), config, clock.clone());
    assert_eq!(
        reopened
            .runtime_db()
            .provider_queue_parking("default", &message.id)
            .unwrap(),
        Some(parking.clone())
    );
    clock.advance((parking.not_before - clock.now()).to_std().unwrap());
    let due = reopened
        .runtime_db()
        .begin_provider_recovery_probe_epoch(&parked, clock.now())
        .unwrap()
        .unwrap();
    assert_eq!(due.request_count, 0);
    assert_eq!(due.total_requests, 1);
    assert!(matches!(
        reopened
            .select_provider_turn_candidate(
                &message,
                &attempt,
                &inputs,
                &protocol,
                &chain,
                &due,
                &Default::default(),
            )
            .unwrap(),
        ProviderTurnGate::Granted(_)
    ));
}
