#[path = "support/scheduler_workitem_mvp.rs"]
mod model;

use std::collections::BTreeMap;
use std::path::PathBuf;

use model::{
    assert_invariants, reduce, ActivationSlot, AdmissionCause, AgentDispatchState, Decision, Event,
    Settlement, Snapshot, WaitRecord, WaitState, WorkDemand, WorkStatus,
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
    #[serde(default)]
    focus: Option<String>,
    work: BTreeMap<String, WorkDemand>,
    #[serde(default)]
    waits: BTreeMap<String, WaitRecord>,
    #[serde(default)]
    admitted_revisions: Vec<String>,
    #[serde(default)]
    settled_activations: Vec<String>,
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
            snapshot.admitted_revisions.into_iter().collect::<Vec<_>>(),
            fixture.expected.admitted_revisions,
            "{}: admitted revisions",
            fixture.name
        );
        assert_eq!(
            snapshot.settled_activations.into_iter().collect::<Vec<_>>(),
            fixture.expected.settled_activations,
            "{}: settled activations",
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
            expected_revision: 2,
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
        revision in 1_u64..20,
        expected_revision in 0_u64..22,
        reserved in any::<bool>(),
    ) {
        let mut snapshot = minimal_snapshot(revision);
        if reserved {
            snapshot.work.get_mut("w1").expect("work").status =
                WorkStatus::Waiting { wait_id: "wait-1".into() };
            snapshot.waits.insert(
                "wait-1".into(),
                model::WaitRecord {
                    owner_work_item_id: "w1".into(),
                    generation: 1,
                    state: WaitState::Active,
                    resolved_generations: Default::default(),
                },
            );
            snapshot.dispatch = AgentDispatchState::ReservedFor {
                wait_id: "wait-1".into(),
            };
        }
        let event = Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_revision,
            cause: model::AdmissionCause::Scheduling,
        };
        prop_assert_eq!(reduce(&snapshot, &event), reduce(&snapshot, &event));
    }

    #[test]
    fn stale_revision_never_acquires_the_single_activation_slot(
        revision in 1_u64..100,
        delta in 1_u64..100,
    ) {
        let snapshot = minimal_snapshot(revision);
        let event = Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_revision: revision.saturating_add(delta),
            cause: model::AdmissionCause::Scheduling,
        };
        let outcome = reduce(&snapshot, &event);
        prop_assert_eq!(outcome.decision, Decision::Rejected);
        prop_assert_eq!(outcome.snapshot.slot, ActivationSlot::Idle);
        prop_assert!(outcome.snapshot.admitted_revisions.is_empty());
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
        AgentDispatchState::ReservedFor {
            wait_id: "wait-1".into(),
        },
    ];
    let events = [
        Event::TriggerWait {
            wait_id: "wait-1".into(),
            generation: 1,
        },
        Event::OperatorIntervention {
            input_id: "input-1".into(),
        },
        Event::Admit {
            activation_id: "a1".into(),
            work_item_id: "w1".into(),
            expected_revision: 1,
            cause: model::AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                generation: 1,
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
                model::WaitRecord {
                    owner_work_item_id: "w1".into(),
                    generation: 1,
                    state: wait_state.clone(),
                    resolved_generations: Default::default(),
                },
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
            expected_revision: 1,
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
    assert_eq!(first_wait.snapshot.waits["wait-1"].generation, 2);

    let first_trigger = reduce(
        &first_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            generation: 2,
        },
    );
    assert_eq!(first_trigger.decision, Decision::WaitTriggered);
    let resumed = reduce(
        &first_trigger.snapshot,
        &Event::Admit {
            activation_id: "a2".into(),
            work_item_id: "w1".into(),
            expected_revision: 2,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                generation: 2,
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
    assert_eq!(rearmed_wait.generation, 3);
    assert_eq!(rearmed_wait.state, WaitState::Active);
    assert_eq!(
        rearmed_wait
            .resolved_generations
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [2]
    );
    assert!(reused_wait
        .transitions
        .contains(&"wait:wait-1:generation:2:consumed->resolved".into()));

    let stale_trigger = reduce(
        &reused_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            generation: 2,
        },
    );
    assert_eq!(stale_trigger.decision, Decision::Rejected);
    assert_eq!(stale_trigger.diagnostics, ["stale_wait_generation"]);

    let triggered = reduce(
        &reused_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            generation: 3,
        },
    );
    assert_eq!(triggered.decision, Decision::WaitTriggered);

    let stale_resume = reduce(
        &triggered.snapshot,
        &Event::Admit {
            activation_id: "a-stale".into(),
            work_item_id: "w1".into(),
            expected_revision: 3,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                generation: 2,
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
            expected_revision: 3,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                generation: 3,
            },
        },
    );
    assert_eq!(current_resume.decision, Decision::Admitted);
}

#[test]
fn activation_cannot_settle_after_work_item_revision_changes() {
    let mut snapshot = minimal_snapshot(4);
    snapshot.slot = ActivationSlot::Running {
        activation_id: "a1".into(),
        work_item_id: "w1".into(),
        admitted_revision: 4,
    };
    snapshot.work.get_mut("w1").expect("work").revision = 5;

    let outcome = reduce(
        &snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Continue,
        },
    );
    assert_eq!(outcome.decision, Decision::Rejected);
    assert_eq!(outcome.diagnostics, ["stale_activation_revision"]);
    assert_eq!(outcome.snapshot, snapshot);
}

fn minimal_snapshot(revision: u64) -> Snapshot {
    Snapshot {
        slot: ActivationSlot::Idle,
        dispatch: AgentDispatchState::Open,
        focus: Some("w1".into()),
        work: BTreeMap::from([(
            "w1".into(),
            model::WorkDemand {
                revision,
                status: WorkStatus::Runnable,
                capabilities: ["workspace_write".into()].into_iter().collect(),
                locks: ["workspace:holon".into()].into_iter().collect(),
                locality: "workspace:holon".into(),
                cost_class: "standard".into(),
            },
        )]),
        waits: BTreeMap::new(),
        admitted_revisions: Default::default(),
        settled_activations: Default::default(),
        continuation_admissions: Default::default(),
    }
}
