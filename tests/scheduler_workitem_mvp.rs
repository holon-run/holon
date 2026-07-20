#[path = "support/scheduler_workitem_mvp.rs"]
mod model;

use std::collections::BTreeMap;
use std::path::PathBuf;

use model::{
    assert_invariants, reduce, ActivationSlot, AgentDispatchState, Decision, Event, Snapshot,
    WaitState, WorkStatus,
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
    #[serde(default)]
    work_status: BTreeMap<String, WorkStatus>,
    #[serde(default)]
    work_revision: BTreeMap<String, u64>,
    #[serde(default)]
    wait_state: BTreeMap<String, WaitState>,
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
        for (work_item_id, status) in fixture.expected.work_status {
            assert_eq!(
                snapshot.work.get(&work_item_id).map(|work| &work.status),
                Some(&status),
                "{}: work status for {work_item_id}",
                fixture.name
            );
        }
        for (work_item_id, revision) in fixture.expected.work_revision {
            assert_eq!(
                snapshot.work.get(&work_item_id).map(|work| work.revision),
                Some(revision),
                "{}: work revision for {work_item_id}",
                fixture.name
            );
        }
        for (wait_id, state) in fixture.expected.wait_state {
            assert_eq!(
                snapshot.waits.get(&wait_id).map(|wait| &wait.state),
                Some(&state),
                "{}: wait state for {wait_id}",
                fixture.name
            );
        }
        for activation_id in fixture.expected.settled_activations {
            assert!(
                snapshot.settled_activations.contains(&activation_id),
                "{}: missing settled activation {activation_id}",
                fixture.name
            );
        }
        for admission_id in fixture.expected.continuation_admissions {
            assert!(
                snapshot.continuation_admissions.contains(&admission_id),
                "{}: missing continuation admission {admission_id}",
                fixture.name
            );
        }
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
    let replay = reduce(&reloaded, &fixture.events[0]);
    assert_eq!(replay.decision, Decision::Rejected);
    assert_eq!(replay.snapshot, reloaded);
    assert_eq!(replay.diagnostics, ["no_running_activation"]);
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
                    state: WaitState::Active,
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
                    state: wait_state.clone(),
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
            },
        )]),
        waits: BTreeMap::new(),
        admitted_revisions: Default::default(),
        settled_activations: Default::default(),
        continuation_admissions: Default::default(),
    }
}
