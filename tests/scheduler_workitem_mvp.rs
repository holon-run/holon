#[path = "support/scheduler_workitem_mvp.rs"]
mod model;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use model::{
    assert_invariants, reduce, ActivationRecord, ActivationSlot, ActivationState, AdmissionCause,
    AgentDispatchState, Decision, Event, ObservationalDivergenceAllowance, ProtocolMode,
    RollbackAction, RollbackPolicy, RollbackTrigger, RolloutClassEvidence, RolloutManifest,
    RolloutPreflightState, RolloutState, ScenarioMode, Settlement, Snapshot, WaitGenerationRecord,
    WaitRecord, WaitState, WaitTrigger, WorkDemand, WorkStatus,
};
use proptest::prelude::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    initial: Snapshot,
    events: Vec<Event>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
struct Expected {
    decisions: Vec<Decision>,
    slot: ActivationSlot,
    dispatch: AgentDispatchState,
    dispatch_revision: u64,
    #[serde(default)]
    focus: Option<String>,
    work: BTreeMap<String, WorkDemand>,
    #[serde(default)]
    waits: BTreeMap<String, WaitRecord>,
    #[serde(default)]
    activations: BTreeMap<String, ActivationRecord>,
    rollout: RolloutState,
    #[serde(default)]
    admitted_generations: Vec<String>,
    #[serde(default)]
    continuation_admissions: Vec<String>,
}

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/scheduler_workitem_mvp/scenarios.json")
}

fn fixtures() -> Vec<Fixture> {
    let path = fixture_path();
    let content = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&content)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

#[test]
fn historical_scenarios_replay_to_one_explicit_state() {
    for fixture in fixtures() {
        let mut snapshot = fixture.initial;
        let mut decisions = Vec::new();
        for event in &fixture.events {
            let first = reduce(&snapshot, event);
            let second = reduce(&snapshot, event);
            assert_eq!(
                first, second,
                "{}: reducer must be deterministic for {event:?}",
                fixture.name
            );
            decisions.push(first.decision.clone());
            snapshot = first.snapshot;
            assert_invariants(&snapshot)
                .unwrap_or_else(|error| panic!("{}: invariant failed: {error}", fixture.name));
        }

        assert_eq!(
            decisions, fixture.expected.decisions,
            "{}: decisions",
            fixture.name
        );
        assert_eq!(
            snapshot.slot, fixture.expected.slot,
            "{}: slot",
            fixture.name
        );
        assert_eq!(
            snapshot.dispatch, fixture.expected.dispatch,
            "{}: dispatch",
            fixture.name
        );
        assert_eq!(
            snapshot.dispatch_revision, fixture.expected.dispatch_revision,
            "{}: dispatch revision",
            fixture.name
        );
        assert_eq!(
            snapshot.focus, fixture.expected.focus,
            "{}: focus",
            fixture.name
        );
        assert_eq!(
            snapshot.work, fixture.expected.work,
            "{}: work",
            fixture.name
        );
        assert_eq!(
            snapshot.waits, fixture.expected.waits,
            "{}: waits",
            fixture.name
        );
        assert_eq!(
            snapshot.activations, fixture.expected.activations,
            "{}: activations",
            fixture.name
        );
        assert_eq!(
            snapshot.rollout, fixture.expected.rollout,
            "{}: rollout",
            fixture.name
        );
        assert_eq!(
            snapshot
                .admitted_generations
                .into_iter()
                .collect::<Vec<_>>(),
            fixture.expected.admitted_generations,
            "{}: admitted generations",
            fixture.name
        );
        assert_eq!(
            snapshot
                .continuation_admissions
                .into_iter()
                .collect::<Vec<_>>(),
            fixture.expected.continuation_admissions,
            "{}: continuation admissions",
            fixture.name
        );
    }
}

#[test]
fn serialized_snapshot_does_not_replay_a_settled_activation() {
    let fixture = fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "serialized_snapshot_fences_settled_activation")
        .expect("fixture");
    let first = reduce(&fixture.initial, &fixture.events[0]);
    let encoded = serde_json::to_vec(&first.snapshot).expect("serialize");
    let reloaded: Snapshot = serde_json::from_slice(&encoded).expect("deserialize");
    let replay = reduce(
        &reloaded,
        &Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_generation: 2,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    assert_eq!(replay.decision, Decision::Rejected);
    assert_eq!(replay.snapshot, reloaded);
    assert_eq!(replay.diagnostics, ["activation_already_settled"]);
}

proptest! {
    #[test]
    fn identical_snapshot_and_event_always_produce_identical_outcome(
        scheduling_generation in 1_u64..20,
        expected_generation in 0_u64..22,
        reserved in any::<bool>(),
    ) {
        let mut snapshot = minimal_snapshot(scheduling_generation);
        if reserved {
            snapshot.work.get_mut("w1").expect("work").status =
                WorkStatus::Waiting { wait_id: "wait-1".into() };
            snapshot.waits.insert(
                "wait-1".into(),
                wait_record(
                    "w1",
                    scheduling_generation,
                    WaitState::Active,
                    None,
                    None,
                ),
            );
            snapshot.dispatch = AgentDispatchState::Awaiting {
                wait_id: "wait-1".into(),
            };
        }
        let event = Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_generation,
            expected_dispatch_revision: snapshot.dispatch_revision,
            cause: model::AdmissionCause::Scheduling,
        };
        prop_assert_eq!(reduce(&snapshot, &event), reduce(&snapshot, &event));
    }

    #[test]
    fn stale_generation_never_acquires_the_single_activation_slot(
        scheduling_generation in 1_u64..100,
        delta in 1_u64..100,
    ) {
        let snapshot = minimal_snapshot(scheduling_generation);
        let event = Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_generation: scheduling_generation.saturating_add(delta),
            expected_dispatch_revision: 0,
            cause: model::AdmissionCause::Scheduling,
        };
        let outcome = reduce(&snapshot, &event);
        prop_assert_eq!(outcome.decision, Decision::Rejected);
        prop_assert_eq!(outcome.snapshot.slot, ActivationSlot::Idle);
        prop_assert!(outcome.snapshot.admitted_generations.is_empty());
    }
}

#[test]
fn small_state_space_preserves_lane_wait_and_settlement_invariants() {
    let wait_states = [
        WaitState::Active,
        WaitState::Triggered,
        WaitState::Consumed,
        WaitState::Resolved,
    ];
    let dispatch_states = [
        AgentDispatchState::Open,
        AgentDispatchState::Awaiting {
            wait_id: "wait-1".into(),
        },
    ];
    let events = [
        Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 1,
            trigger_id: "trigger-1".into(),
            trigger_generation: 1,
        },
        Event::OperatorIntervention {
            input_id: "input-1".into(),
        },
        Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: model::AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                wait_generation: 1,
                trigger_id: "trigger-1".into(),
                trigger_generation: 1,
            },
        },
    ];

    for wait_state in wait_states {
        for dispatch in &dispatch_states {
            let mut snapshot = minimal_snapshot(1);
            snapshot.work.get_mut("w1").expect("work").status = WorkStatus::Waiting {
                wait_id: "wait-1".into(),
            };
            snapshot.waits.insert(
                "wait-1".into(),
                wait_record(
                    "w1",
                    1,
                    wait_state.clone(),
                    (!matches!(wait_state, WaitState::Active)).then_some(("trigger-1", 1)),
                    (wait_state == WaitState::Consumed).then_some("a1"),
                ),
            );
            snapshot.dispatch = dispatch.clone();

            if assert_invariants(&snapshot).is_err() {
                continue;
            }
            for event in &events {
                let outcome = reduce(&snapshot, event);
                assert_invariants(&outcome.snapshot).expect("reducer emitted invalid state");
                if wait_state != WaitState::Triggered && matches!(event, Event::Admit { .. }) {
                    assert_eq!(outcome.decision, Decision::Rejected);
                }
                if wait_state != WaitState::Active && matches!(event, Event::TriggerWait { .. }) {
                    assert_eq!(outcome.decision, Decision::DuplicateIgnored);
                }
            }
        }
    }
}

#[test]
fn stale_wait_generation_cannot_trigger_or_resume_reused_wait_id() {
    let first_activation = reduce(
        &minimal_snapshot(1),
        &Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    assert_eq!(first_activation.decision, Decision::Admitted);
    let first_wait = reduce(
        &first_activation.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Wait {
                wait_id: "wait-1".into(),
                mode: model::WaitMode::AwaitThis,
            },
        },
    );
    assert_eq!(first_wait.decision, Decision::Settled);
    assert_eq!(first_wait.snapshot.waits["wait-1"].current_generation, 2);

    let first_trigger = reduce(
        &first_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 2,
            trigger_id: "trigger-2".into(),
            trigger_generation: 1,
        },
    );
    assert_eq!(first_trigger.decision, Decision::WaitTriggered);
    let resumed = reduce(
        &first_trigger.snapshot,
        &Event::Admit {
            activation_id: "a2".into(),
            work_item_id: "w1".into(),
            expected_generation: 2,
            expected_dispatch_revision: 1,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                wait_generation: 2,
                trigger_id: "trigger-2".into(),
                trigger_generation: 1,
            },
        },
    );
    assert_eq!(resumed.decision, Decision::Admitted);
    let reused_wait = reduce(
        &resumed.snapshot,
        &Event::Settle {
            activation_id: "a2".into(),
            settlement: Settlement::Wait {
                wait_id: "wait-1".into(),
                mode: model::WaitMode::AwaitThis,
            },
        },
    );
    assert_eq!(reused_wait.decision, Decision::Settled);
    let rearmed_wait = &reused_wait.snapshot.waits["wait-1"];
    assert_eq!(
        rearmed_wait.current_generation, 3,
        "rearm must advance the canonical wait generation"
    );
    assert_eq!(rearmed_wait.generations[&2].state, WaitState::Resolved);
    assert_eq!(rearmed_wait.generations[&3].state, WaitState::Active);
    assert!(reused_wait
        .transitions
        .contains(&"wait:wait-1:generation:2:consumed->resolved".into()));

    let stale_trigger = reduce(
        &reused_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 2,
            trigger_id: "trigger-stale".into(),
            trigger_generation: 1,
        },
    );
    assert_eq!(stale_trigger.decision, Decision::Rejected);
    assert_eq!(stale_trigger.diagnostics, ["stale_wait_generation"]);

    let triggered = reduce(
        &reused_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 3,
            trigger_id: "trigger-3".into(),
            trigger_generation: 1,
        },
    );
    assert_eq!(triggered.decision, Decision::WaitTriggered);

    let stale_resume = reduce(
        &triggered.snapshot,
        &Event::Admit {
            activation_id: "a-stale".into(),
            work_item_id: "w1".into(),
            expected_generation: 3,
            expected_dispatch_revision: 3,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                wait_generation: 2,
                trigger_id: "trigger-2".into(),
                trigger_generation: 1,
            },
        },
    );
    assert_eq!(stale_resume.decision, Decision::Rejected);
    assert_eq!(stale_resume.diagnostics, ["stale_wait_generation"]);

    let current_resume = reduce(
        &triggered.snapshot,
        &Event::Admit {
            activation_id: "a3".into(),
            work_item_id: "w1".into(),
            expected_generation: 3,
            expected_dispatch_revision: 3,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                wait_generation: 3,
                trigger_id: "trigger-3".into(),
                trigger_generation: 1,
            },
        },
    );
    assert_eq!(current_resume.decision, Decision::Admitted);
}

#[test]
fn activation_cannot_settle_after_scheduling_generation_changes() {
    let mut snapshot = minimal_snapshot(4);
    snapshot.slot = ActivationSlot::Running {
        activation_id: "a1".into(),
        work_item_id: "w1".into(),
        admitted_generation: 4,
        recovery_for: None,
    };
    snapshot
        .work
        .get_mut("w1")
        .expect("work")
        .scheduling_generation = 5;

    let outcome = reduce(
        &snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Continue,
        },
    );
    assert_eq!(outcome.decision, Decision::Rejected);
    assert_eq!(outcome.diagnostics, ["stale_activation_generation"]);
    assert_eq!(outcome.snapshot, snapshot);
}

#[test]
fn metadata_revision_changes_do_not_invalidate_activation_generation() {
    let admitted = reduce(
        &minimal_snapshot(4),
        &Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_generation: 4,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let metadata_updated = reduce(
        &admitted.snapshot,
        &Event::UpdateMetadata {
            work_item_id: "w1".into(),
            expected_metadata_revision: 4,
        },
    );
    assert_eq!(metadata_updated.decision, Decision::MetadataUpdated);

    let settled = reduce(
        &metadata_updated.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Continue,
        },
    );
    assert_eq!(settled.decision, Decision::Settled);
}

#[test]
fn settlement_recovery_success_closes_both_activations() {
    let admitted = reduce(
        &minimal_snapshot(7),
        &Event::Admit {
            activation_id: "a-missing".into(),
            work_item_id: "w1".into(),
            expected_generation: 7,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = reduce(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a-missing".into(),
            settlement: Settlement::Missing,
        },
    );
    assert_eq!(missing.decision, Decision::SettlementMissing);
    assert_invariants(&missing.snapshot).expect("missing settlement must be canonical");

    let recovery = reduce(
        &missing.snapshot,
        &Event::Admit {
            activation_id: "a-recovery".into(),
            work_item_id: "w1".into(),
            expected_generation: 7,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::SettlementRecovery {
                missing_activation_id: "a-missing".into(),
            },
        },
    );
    assert_eq!(recovery.decision, Decision::Admitted);
    assert_invariants(&recovery.snapshot).expect("recovery activation must be canonical");

    let settled = reduce(
        &recovery.snapshot,
        &Event::Settle {
            activation_id: "a-recovery".into(),
            settlement: Settlement::Continue,
        },
    );
    assert_eq!(settled.decision, Decision::Settled);
    assert_eq!(settled.snapshot.slot, ActivationSlot::Idle);
    assert_eq!(settled.snapshot.work["w1"].scheduling_generation, 8);
    assert_eq!(settled.snapshot.work["w1"].status, WorkStatus::Runnable);
    assert_eq!(
        settled.snapshot.activations["a-missing"].state,
        ActivationState::Settled
    );
    assert_eq!(
        settled.snapshot.activations["a-recovery"].state,
        ActivationState::Settled
    );
    assert!(settled
        .transitions
        .contains(&"settlement:a-missing:recovered:a-recovery".into()));
    assert_invariants(&settled.snapshot).expect("successful recovery must preserve invariants");
}

#[test]
fn failed_settlement_recovery_enters_typed_hold_and_cannot_retry() {
    let admitted = reduce(
        &minimal_snapshot(11),
        &Event::Admit {
            activation_id: "a-missing".into(),
            work_item_id: "w1".into(),
            expected_generation: 11,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = reduce(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a-missing".into(),
            settlement: Settlement::Missing,
        },
    );
    let recovery = reduce(
        &missing.snapshot,
        &Event::Admit {
            activation_id: "a-recovery".into(),
            work_item_id: "w1".into(),
            expected_generation: 11,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::SettlementRecovery {
                missing_activation_id: "a-missing".into(),
            },
        },
    );

    let held = reduce(
        &recovery.snapshot,
        &Event::Settle {
            activation_id: "a-recovery".into(),
            settlement: Settlement::Missing,
        },
    );
    assert_eq!(held.decision, Decision::SettlementHeld);
    assert_eq!(held.snapshot.slot, ActivationSlot::Idle);
    assert_eq!(held.snapshot.work["w1"].scheduling_generation, 11);
    assert_eq!(
        held.snapshot.work["w1"].status,
        WorkStatus::Paused {
            hold_id: "settlement-recovery:a-missing".into(),
        }
    );
    assert_eq!(
        held.snapshot.activations["a-missing"].state,
        ActivationState::SettlementMissing
    );
    assert_eq!(
        held.snapshot.activations["a-recovery"],
        ActivationRecord {
            work_item_id: "w1".into(),
            admitted_generation: 11,
            state: ActivationState::SettlementMissing,
            recovery_for: Some("a-missing".into()),
        }
    );
    assert_invariants(&held.snapshot).expect("failed recovery must preserve typed hold invariants");

    let retry = reduce(
        &held.snapshot,
        &Event::Admit {
            activation_id: "a-retry".into(),
            work_item_id: "w1".into(),
            expected_generation: 11,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::SettlementRecovery {
                missing_activation_id: "a-missing".into(),
            },
        },
    );
    assert_eq!(retry.decision, Decision::Rejected);
    assert_eq!(retry.diagnostics, ["settlement_recovery_already_attempted"]);
    assert_eq!(retry.snapshot, held.snapshot);
}

#[test]
fn stale_dispatch_revision_cannot_claim_the_lane() {
    let mut snapshot = minimal_snapshot(3);
    snapshot.dispatch_revision = 2;

    let stale = reduce(
        &snapshot,
        &Event::Admit {
            activation_id: "a-stale-dispatch".into(),
            work_item_id: "w1".into(),
            expected_generation: 3,
            expected_dispatch_revision: 1,
            cause: AdmissionCause::Scheduling,
        },
    );
    assert_eq!(stale.decision, Decision::Rejected);
    assert_eq!(stale.diagnostics, ["stale_dispatch_revision"]);
    assert_eq!(stale.snapshot, snapshot);
}

#[test]
fn pending_settlement_recovery_blocks_ordinary_work_admission() {
    let admitted = reduce(
        &minimal_snapshot(5),
        &Event::Admit {
            activation_id: "a-missing".into(),
            work_item_id: "w1".into(),
            expected_generation: 5,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = reduce(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a-missing".into(),
            settlement: Settlement::Missing,
        },
    );
    let mut snapshot = missing.snapshot;
    snapshot.work.insert(
        "w2".into(),
        WorkDemand {
            metadata_revision: 1,
            scheduling_generation: 1,
            status: WorkStatus::Runnable,
            capabilities: BTreeSet::new(),
            locks: BTreeSet::new(),
            locality: "workspace:holon".into(),
            cost_class: "standard".into(),
        },
    );

    let ordinary = reduce(
        &snapshot,
        &Event::Admit {
            activation_id: "a-w2".into(),
            work_item_id: "w2".into(),
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    assert_eq!(ordinary.decision, Decision::Rejected);
    assert_eq!(ordinary.diagnostics, ["settlement_recovery_pending"]);
    assert_eq!(ordinary.snapshot, snapshot);
}

#[test]
fn rollout_authority_requires_complete_evidence_and_fenced_rollback() {
    let snapshot = minimal_snapshot(1);
    let mut incomplete = rollout_manifest();
    incomplete
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .verified_evidence
        .remove("rollback_drill");
    let rejected = reduce(
        &snapshot,
        &Event::InstallRolloutManifest {
            expected_config_revision: 0,
            manifest: incomplete,
        },
    );
    assert_eq!(rejected.decision, Decision::Rejected);
    assert_eq!(rejected.diagnostics, ["rollout_manifest_incomplete"]);

    let mut empty_required = rollout_manifest();
    empty_required
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .required_evidence
        .clear();
    assert_rollout_manifest_rejected(&snapshot, empty_required);

    let mut omitted_mandatory_evidence = rollout_manifest();
    let class = omitted_mandatory_evidence
        .classes
        .get_mut("exact_wait_resume")
        .expect("class");
    class.required_evidence.remove("duplicate_trigger");
    class.verified_evidence.remove("duplicate_trigger");
    assert_rollout_manifest_rejected(&snapshot, omitted_mandatory_evidence);

    let mut insufficient_samples = rollout_manifest();
    let class = insufficient_samples
        .classes
        .get_mut("exact_wait_resume")
        .expect("class");
    class.minimum_shadow_samples = 999;
    class.observed_shadow_samples = 999;
    assert_rollout_manifest_rejected(&snapshot, insufficient_samples);

    let mut insufficient_duration = rollout_manifest();
    let class = insufficient_duration
        .classes
        .get_mut("exact_wait_resume")
        .expect("class");
    class.minimum_shadow_duration_secs = 604_799;
    class.observed_shadow_duration_secs = 604_799;
    assert_rollout_manifest_rejected(&snapshot, insufficient_duration);

    let mut excessive_p99_budget = rollout_manifest();
    excessive_p99_budget
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .maximum_p99_latency_regression_bps = 1_001;
    assert_rollout_manifest_rejected(&snapshot, excessive_p99_budget);

    let mut excessive_observed_p99 = rollout_manifest();
    excessive_observed_p99
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .observed_p99_latency_regression_bps = 501;
    assert_rollout_manifest_rejected(&snapshot, excessive_observed_p99);

    let mut safety_divergence = rollout_manifest();
    safety_divergence.safety_divergence_bps = 1;
    assert_rollout_manifest_rejected(&snapshot, safety_divergence);

    let mut canonical_divergence = rollout_manifest();
    canonical_divergence.canonical_state_divergence_bps = 1;
    assert_rollout_manifest_rejected(&snapshot, canonical_divergence);

    let mut unreviewed_observational_divergence = rollout_manifest();
    unreviewed_observational_divergence
        .allowed_observational_divergence
        .get_mut("diagnostic_order")
        .expect("allowance")
        .reviewed_by
        .clear();
    assert_rollout_manifest_rejected(&snapshot, unreviewed_observational_divergence);

    let mut excessive_observational_divergence = rollout_manifest();
    excessive_observational_divergence
        .allowed_observational_divergence
        .get_mut("diagnostic_order")
        .expect("allowance")
        .maximum_rate_bps = 101;
    assert_rollout_manifest_rejected(&snapshot, excessive_observational_divergence);

    let mut missing_rollback_trigger = rollout_manifest();
    missing_rollback_trigger
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .rollback_policy
        .action = RollbackAction::StopAdmissionsAndRevert {
        target: ScenarioMode::Authoritative,
    };
    assert_rollout_manifest_rejected(&snapshot, missing_rollback_trigger);

    for configured_mode in [ScenarioMode::Shadow, ScenarioMode::Off] {
        let mut invalid_rollback_target = rollout_manifest();
        let class = invalid_rollback_target
            .classes
            .get_mut("exact_wait_resume")
            .expect("class");
        class.configured_mode = configured_mode;
        class.rollback_policy.action = RollbackAction::StopAdmissionsAndRevert {
            target: ScenarioMode::Authoritative,
        };
        assert_rollout_manifest_rejected(&snapshot, invalid_rollback_target);
    }

    let (preflight_ready, manifest) = complete_rollout_preflight(&snapshot, rollout_manifest());
    let installed = reduce(
        &preflight_ready,
        &Event::InstallRolloutManifest {
            expected_config_revision: 0,
            manifest,
        },
    );
    assert_eq!(installed.decision, Decision::ManifestInstalled);

    let mut stale_preflight_binding = rollout_manifest();
    stale_preflight_binding.revision = 2;
    assert_rollout_manifest_rejected(&installed.snapshot, stale_preflight_binding);

    let unchanged_new_revision = next_rollout_manifest();
    assert_rollout_preflight_refresh_required(&installed.snapshot, unchanged_new_revision);

    let mut changed_threshold = next_rollout_manifest();
    let class = changed_threshold
        .classes
        .get_mut("exact_wait_resume")
        .expect("class");
    class.minimum_shadow_samples += 1;
    class.observed_shadow_samples += 1;
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_threshold);

    let mut changed_corpus = next_rollout_manifest();
    changed_corpus.fixture_corpus_revision = "scheduler-workitem-phase0-v2".into();
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_corpus);

    let mut changed_divergence_allowance = next_rollout_manifest();
    changed_divergence_allowance
        .allowed_observational_divergence
        .get_mut("diagnostic_order")
        .expect("allowance")
        .maximum_rate_bps += 1;
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_divergence_allowance);

    let mut changed_classification = next_rollout_manifest();
    changed_classification
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .configured_mode = ScenarioMode::Shadow;
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_classification);

    let mut changed_required_evidence = next_rollout_manifest();
    let class = changed_required_evidence
        .classes
        .get_mut("exact_wait_resume")
        .expect("class");
    class.required_evidence.insert("operator_signoff".into());
    class.verified_evidence.insert("operator_signoff".into());
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_required_evidence);

    let mut changed_latency_budget = next_rollout_manifest();
    changed_latency_budget
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .maximum_p99_latency_regression_bps = 499;
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_latency_budget);

    let mut changed_rollback_policy = next_rollout_manifest();
    changed_rollback_policy
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .rollback_policy
        .action = RollbackAction::StopAdmissionsAndRevert {
        target: ScenarioMode::Off,
    };
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_rollback_policy);

    let mut changed_build_and_schema = next_rollout_manifest();
    changed_build_and_schema.protocol_build = "holon-0.30.1-test".into();
    changed_build_and_schema.schema_build = "scheduler-protocol-schema-v2".into();
    changed_build_and_schema.schema_revision = 2;
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_build_and_schema);

    let mut changed_observation_results = next_rollout_manifest();
    let class = changed_observation_results
        .classes
        .get_mut("exact_wait_resume")
        .expect("class");
    class.observed_shadow_samples += 1;
    class.observed_shadow_duration_secs += 1;
    class.observed_p99_latency_regression_bps += 1;
    assert_rollout_preflight_refresh_required(&installed.snapshot, changed_observation_results);

    let failed_opened = reduce(
        &installed.snapshot,
        &Event::OpenRolloutPreflight {
            expected_config_revision: 1,
            manifest_revision: 2,
        },
    );
    assert_eq!(failed_opened.decision, Decision::RolloutPreflightOpened);
    let failed_preflight_revision = failed_opened.snapshot.rollout.latest_preflight_revision;
    let mut failed_preflight = next_rollout_manifest();
    failed_preflight.preflight_revision = failed_preflight_revision;
    failed_preflight.preflight_for_manifest_revision = failed_preflight.revision;
    failed_preflight.preflight_succeeded = false;
    let failed_completion = reduce(
        &failed_opened.snapshot,
        &Event::CompleteRolloutPreflight {
            expected_config_revision: 1,
            expected_preflight_revision: failed_preflight_revision,
            manifest: failed_preflight.clone(),
        },
    );
    assert_eq!(failed_completion.decision, Decision::Rejected);
    assert_eq!(failed_completion.diagnostics, ["rollout_preflight_failed"]);
    assert_eq!(failed_completion.snapshot, failed_opened.snapshot);

    let mut forged_completed = failed_opened.snapshot.clone();
    let forged_record = forged_completed
        .rollout
        .preflights
        .get_mut(&failed_preflight_revision)
        .expect("failed preflight record");
    forged_record.state = RolloutPreflightState::Completed;
    forged_record.manifest = Some(failed_preflight.clone());
    let failed_installation = reduce(
        &forged_completed,
        &Event::InstallRolloutManifest {
            expected_config_revision: 1,
            manifest: failed_preflight,
        },
    );
    assert_eq!(failed_installation.decision, Decision::Rejected);
    assert_eq!(
        failed_installation.diagnostics,
        ["rollout_preflight_failed"]
    );
    assert_eq!(failed_installation.snapshot, forged_completed);

    let mut replayed_observation = installed
        .snapshot
        .rollout
        .manifest
        .clone()
        .expect("installed manifest");
    replayed_observation.revision = 2;
    replayed_observation.preflight_revision = 2;
    replayed_observation.preflight_for_manifest_revision = 2;
    let replayed = reduce(
        &installed.snapshot,
        &Event::InstallRolloutManifest {
            expected_config_revision: 1,
            manifest: replayed_observation,
        },
    );
    assert_eq!(replayed.decision, Decision::Rejected);
    assert_eq!(replayed.diagnostics, ["rollout_preflight_record_missing"]);

    let opened_only = reduce(
        &installed.snapshot,
        &Event::OpenRolloutPreflight {
            expected_config_revision: 1,
            manifest_revision: 2,
        },
    );
    assert_eq!(opened_only.decision, Decision::RolloutPreflightOpened);
    let mut uncompleted_observation = next_rollout_manifest();
    uncompleted_observation.preflight_revision = 2;
    let uncompleted = reduce(
        &opened_only.snapshot,
        &Event::InstallRolloutManifest {
            expected_config_revision: 1,
            manifest: uncompleted_observation,
        },
    );
    assert_eq!(uncompleted.decision, Decision::Rejected);
    assert_eq!(
        uncompleted.diagnostics,
        ["rollout_preflight_record_not_installable"]
    );

    let mut refreshed_preflight = next_rollout_manifest();
    refreshed_preflight.fixture_corpus_revision = "scheduler-workitem-phase0-v2".into();
    let (refreshed_ready, refreshed_preflight) =
        complete_rollout_preflight(&installed.snapshot, refreshed_preflight);
    let mut changed_after_preflight = refreshed_preflight.clone();
    changed_after_preflight
        .classes
        .get_mut("exact_wait_resume")
        .expect("class")
        .observed_shadow_samples += 1;
    let mismatched = reduce(
        &refreshed_ready,
        &Event::InstallRolloutManifest {
            expected_config_revision: 1,
            manifest: changed_after_preflight,
        },
    );
    assert_eq!(mismatched.decision, Decision::Rejected);
    assert_eq!(
        mismatched.diagnostics,
        ["rollout_preflight_record_mismatch"]
    );
    let refreshed = reduce(
        &refreshed_ready,
        &Event::InstallRolloutManifest {
            expected_config_revision: 1,
            manifest: refreshed_preflight,
        },
    );
    assert_eq!(refreshed.decision, Decision::ManifestInstalled);

    let configured = reduce(
        &installed.snapshot,
        &Event::ConfigureProtocol {
            expected_config_revision: 1,
            mode: ProtocolMode::Authoritative,
        },
    );
    assert_eq!(configured.decision, Decision::ProtocolConfigured);
    let off_to_authoritative = reduce(
        &configured.snapshot,
        &Event::ChangeScenarioAuthority {
            scenario_class: "exact_wait_resume".into(),
            expected_config_revision: 2,
            expected_manifest_revision: 1,
            expected_preflight_revision: 1,
            mode: ScenarioMode::Authoritative,
        },
    );
    assert_eq!(off_to_authoritative.decision, Decision::Rejected);
    assert_eq!(off_to_authoritative.diagnostics, ["scenario_not_shadow"]);
    assert_eq!(off_to_authoritative.snapshot, configured.snapshot);

    let shadowed = reduce(
        &configured.snapshot,
        &Event::ChangeScenarioAuthority {
            scenario_class: "exact_wait_resume".into(),
            expected_config_revision: 2,
            expected_manifest_revision: 1,
            expected_preflight_revision: 1,
            mode: ScenarioMode::Shadow,
        },
    );
    assert_eq!(shadowed.decision, Decision::ScenarioAuthorityChanged);
    let authorized = reduce(
        &shadowed.snapshot,
        &Event::ChangeScenarioAuthority {
            scenario_class: "exact_wait_resume".into(),
            expected_config_revision: 3,
            expected_manifest_revision: 1,
            expected_preflight_revision: 1,
            mode: ScenarioMode::Authoritative,
        },
    );
    assert_eq!(authorized.decision, Decision::ScenarioAuthorityChanged);

    let stale = reduce(
        &authorized.snapshot,
        &Event::ReportScenarioHardBlocker {
            scenario_class: "exact_wait_resume".into(),
            blocker_code: "stale_wait_generation_accepted".into(),
            expected_config_revision: 3,
            expected_manifest_revision: 1,
            expected_preflight_revision: 1,
        },
    );
    assert_eq!(stale.decision, Decision::Rejected);
    assert_eq!(stale.diagnostics, ["stale_rollout_config_revision"]);

    let stale_manifest = reduce(
        &authorized.snapshot,
        &Event::ReportScenarioHardBlocker {
            scenario_class: "exact_wait_resume".into(),
            blocker_code: "stale_wait_generation_accepted".into(),
            expected_config_revision: 4,
            expected_manifest_revision: 0,
            expected_preflight_revision: 1,
        },
    );
    assert_eq!(stale_manifest.decision, Decision::Rejected);
    assert_eq!(
        stale_manifest.diagnostics,
        ["stale_rollout_manifest_revision"]
    );

    let stale_preflight = reduce(
        &authorized.snapshot,
        &Event::ReportScenarioHardBlocker {
            scenario_class: "exact_wait_resume".into(),
            blocker_code: "stale_wait_generation_accepted".into(),
            expected_config_revision: 4,
            expected_manifest_revision: 1,
            expected_preflight_revision: 0,
        },
    );
    assert_eq!(stale_preflight.decision, Decision::Rejected);
    assert_eq!(
        stale_preflight.diagnostics,
        ["stale_rollout_preflight_revision"]
    );

    let blocker = Event::ReportScenarioHardBlocker {
        scenario_class: "exact_wait_resume".into(),
        blocker_code: "stale_wait_generation_accepted".into(),
        expected_config_revision: 4,
        expected_manifest_revision: 1,
        expected_preflight_revision: 1,
    };
    let rolled_back = reduce(&authorized.snapshot, &blocker);
    assert_eq!(rolled_back.decision, Decision::RollbackTripped);
    assert_eq!(
        rolled_back.snapshot.rollout.scenarios["exact_wait_resume"].mode,
        ScenarioMode::Shadow
    );
    let recorded = rolled_back
        .snapshot
        .rollout
        .hard_blockers
        .iter()
        .next()
        .expect("hard blocker record");
    assert_eq!(recorded.scenario_class, "exact_wait_resume");
    assert_eq!(recorded.blocker_code, "stale_wait_generation_accepted");
    assert_eq!(recorded.config_revision, 4);
    assert_eq!(recorded.manifest_revision, 1);
    assert_eq!(recorded.preflight_revision, 1);
    assert_eq!(recorded.trigger, RollbackTrigger::AnyHardBlocker);
    assert_eq!(
        recorded.action,
        RollbackAction::StopAdmissionsAndRevert {
            target: ScenarioMode::Shadow,
        }
    );

    let encoded = serde_json::to_vec(&rolled_back.snapshot).expect("serialize rollback snapshot");
    let reloaded: Snapshot =
        serde_json::from_slice(&encoded).expect("deserialize rollback snapshot");
    assert_eq!(reloaded, rolled_back.snapshot);

    let duplicate = reduce(&reloaded, &blocker);
    assert_eq!(duplicate.decision, Decision::Rejected);
    assert_eq!(duplicate.diagnostics, ["stale_rollout_config_revision"]);
    assert_eq!(duplicate.snapshot, reloaded);
}

fn assert_rollout_manifest_rejected(snapshot: &Snapshot, manifest: RolloutManifest) {
    let rejected = reduce(
        snapshot,
        &Event::InstallRolloutManifest {
            expected_config_revision: snapshot.rollout.config_revision,
            manifest,
        },
    );
    assert_eq!(rejected.decision, Decision::Rejected);
    assert_eq!(rejected.diagnostics, ["rollout_manifest_incomplete"]);
    assert_eq!(rejected.snapshot, *snapshot);
}

fn assert_rollout_preflight_refresh_required(snapshot: &Snapshot, manifest: RolloutManifest) {
    let rejected = reduce(
        snapshot,
        &Event::InstallRolloutManifest {
            expected_config_revision: snapshot.rollout.config_revision,
            manifest,
        },
    );
    assert_eq!(rejected.decision, Decision::Rejected);
    assert_eq!(
        rejected.diagnostics,
        ["rollout_preflight_record_not_installable"]
    );
    assert_eq!(rejected.snapshot, *snapshot);
}

fn complete_rollout_preflight(
    snapshot: &Snapshot,
    mut manifest: RolloutManifest,
) -> (Snapshot, RolloutManifest) {
    let opened = reduce(
        snapshot,
        &Event::OpenRolloutPreflight {
            expected_config_revision: snapshot.rollout.config_revision,
            manifest_revision: manifest.revision,
        },
    );
    assert_eq!(opened.decision, Decision::RolloutPreflightOpened);
    let preflight_revision = opened.snapshot.rollout.latest_preflight_revision;
    manifest.preflight_revision = preflight_revision;
    manifest.preflight_for_manifest_revision = manifest.revision;
    let completed = reduce(
        &opened.snapshot,
        &Event::CompleteRolloutPreflight {
            expected_config_revision: snapshot.rollout.config_revision,
            expected_preflight_revision: preflight_revision,
            manifest: manifest.clone(),
        },
    );
    assert_eq!(completed.decision, Decision::RolloutPreflightCompleted);
    (completed.snapshot, manifest)
}

fn rollout_manifest() -> RolloutManifest {
    let evidence: BTreeSet<String> = [
        "restart",
        "fault_injection",
        "rollback_drill",
        "duplicate_trigger",
        "stale_generation",
        "restart_after_consume",
        "rearm",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    RolloutManifest {
        revision: 1,
        preflight_revision: 1,
        preflight_for_manifest_revision: 1,
        preflight_succeeded: true,
        protocol_build: "holon-0.30.0-test".into(),
        schema_build: "scheduler-protocol-schema-v1".into(),
        schema_revision: 1,
        fixture_corpus_revision: "scheduler-workitem-phase0-v1".into(),
        classes: BTreeMap::from([(
            "exact_wait_resume".into(),
            RolloutClassEvidence {
                configured_mode: ScenarioMode::Authoritative,
                minimum_shadow_samples: 1_000,
                minimum_shadow_duration_secs: 604_800,
                observed_shadow_samples: 1_000,
                observed_shadow_duration_secs: 604_800,
                maximum_p99_latency_regression_bps: 500,
                observed_p99_latency_regression_bps: 100,
                hard_blocker_count: 0,
                unresolved_divergence_count: 0,
                required_evidence: evidence.clone(),
                verified_evidence: evidence,
                rollback_policy: RollbackPolicy {
                    trigger: RollbackTrigger::AnyHardBlocker,
                    action: RollbackAction::StopAdmissionsAndRevert {
                        target: ScenarioMode::Shadow,
                    },
                },
            },
        )]),
        safety_divergence_bps: 0,
        canonical_state_divergence_bps: 0,
        allowed_observational_divergence: BTreeMap::from([(
            "diagnostic_order".into(),
            ObservationalDivergenceAllowance {
                maximum_rate_bps: 10,
                reviewed_by: "phase0-reviewer".into(),
            },
        )]),
        approver: "phase0-reviewer".into(),
        approved_at: "2026-07-20T00:00:00Z".into(),
    }
}

fn next_rollout_manifest() -> RolloutManifest {
    let mut manifest = rollout_manifest();
    manifest.revision += 1;
    manifest.preflight_for_manifest_revision = manifest.revision;
    manifest
}

fn wait_record(
    owner_work_item_id: &str,
    generation: u64,
    state: WaitState,
    trigger: Option<(&str, u64)>,
    consuming_activation_id: Option<&str>,
) -> WaitRecord {
    WaitRecord {
        current_generation: generation,
        generations: BTreeMap::from([(
            generation,
            WaitGenerationRecord {
                owner_work_item_id: owner_work_item_id.into(),
                state,
                trigger: trigger.map(|(trigger_id, trigger_generation)| WaitTrigger {
                    trigger_id: trigger_id.into(),
                    trigger_generation,
                }),
                consuming_activation_id: consuming_activation_id.map(Into::into),
            },
        )]),
    }
}

fn minimal_snapshot(scheduling_generation: u64) -> Snapshot {
    Snapshot {
        slot: ActivationSlot::Idle,
        dispatch: AgentDispatchState::Open,
        dispatch_revision: 0,
        focus: Some("w1".into()),
        work: BTreeMap::from([(
            "w1".into(),
            model::WorkDemand {
                metadata_revision: scheduling_generation,
                scheduling_generation,
                status: WorkStatus::Runnable,
                capabilities: ["workspace_write".into()].into_iter().collect(),
                locks: ["workspace:holon".into()].into_iter().collect(),
                locality: "workspace:holon".into(),
                cost_class: "standard".into(),
            },
        )]),
        waits: BTreeMap::new(),
        activations: BTreeMap::new(),
        rollout: Default::default(),
        admitted_generations: Default::default(),
        continuation_admissions: Default::default(),
    }
}
