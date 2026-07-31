#[path = "support/scheduler_workitem_mvp.rs"]
mod model;

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use model::{
    assert_invariants, reduce, reduce_command, ActivationBinding, ActivationCause,
    ActivationDisposition, ActivationLifecycleState, ActivationOrigin, ActivationPriority,
    ActivationProvenance, ActivationRecord, ActivationSettlement, ActivationSlot, ActivationState,
    ActivationTrust, AdmissionCause, AdmitActivationCommand, AdoptActivationWorkStateCommand,
    AgentActivation, AgentDispatchDisposition, AgentDispatchState, Decision, Event,
    LegacyWaitAdoption, MissingSettlementRecord, PreemptionPolicy, ProtocolCommand,
    ProtocolConflictKind, RegisterWorkDemandCommand, SchedulerOwner, SettleActivationCommand,
    Settlement, Snapshot, WaitGenerationRecord, WaitIdentity, WaitRecord, WaitState, WaitTrigger,
    WorkDemand, WorkStatus, YieldContinuationRecord,
};
use proptest::prelude::*;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    initial: Snapshot,
    events: Vec<FixtureInput>,
    expected: Expected,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "surface", rename_all = "snake_case")]
enum FixtureInput {
    Protocol { command: ProtocolCommand },
    Event { event: Event },
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
    waits: BTreeMap<String, WaitRecord>,
    activations: BTreeMap<String, ActivationRecord>,
    activation_admissions: BTreeMap<String, AdmitActivationCommand>,
    settlements: BTreeMap<String, ActivationSettlement>,
    missing_settlements: BTreeMap<String, MissingSettlementRecord>,
    admitted_generations: Vec<String>,
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

fn apply_fixture_input(snapshot: &Snapshot, input: &FixtureInput) -> model::Outcome {
    match input {
        FixtureInput::Protocol { command } => reduce_command(snapshot, command).outcome,
        FixtureInput::Event { event } => reduce(snapshot, event),
    }
}

fn apply_event(snapshot: &Snapshot, event: &Event) -> model::Outcome {
    match event {
        Event::Admit {
            activation_id,
            owner,
            expected_generation,
            expected_dispatch_revision,
            cause,
        } => {
            let work_item_id = owner
                .work_item_id()
                .expect("legacy event test adapter requires a WorkItem owner")
                .to_owned();
            let (typed_cause, binding, origin, trust) = match cause {
                AdmissionCause::Scheduling => (
                    ActivationCause::WorkItemRunnable {
                        work_item_id: work_item_id.clone(),
                        scheduling_generation: *expected_generation,
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
                    ActivationOrigin::System,
                    ActivationTrust::RuntimeInstruction,
                ),
                AdmissionCause::TaskRejoin {
                    task_id,
                    message_id,
                    resume,
                } => (
                    ActivationCause::TaskRejoin {
                        task_id: task_id.clone(),
                        message_id: message_id.clone(),
                        resume: resume.clone(),
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
                    ActivationOrigin::Task,
                    ActivationTrust::RuntimeInstruction,
                ),
                AdmissionCause::OperatorInput { message_id, resume } => (
                    ActivationCause::OperatorInput {
                        message_id: message_id.clone(),
                        resume: resume.clone(),
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
                    ActivationOrigin::Operator,
                    ActivationTrust::OperatorInstruction,
                ),
                AdmissionCause::WaitResume {
                    wait_id,
                    wait_generation,
                    trigger_id,
                    trigger_generation,
                } => (
                    ActivationCause::WaitResume {
                        wait_id: wait_id.clone(),
                        wait_generation: *wait_generation,
                        trigger_id: trigger_id.clone(),
                        trigger_generation: *trigger_generation,
                    },
                    ActivationBinding::WaitOwner {
                        wait_id: wait_id.clone(),
                        owner: SchedulerOwner::WorkItem {
                            work_item_id: work_item_id.clone(),
                        },
                    },
                    ActivationOrigin::System,
                    ActivationTrust::RuntimeInstruction,
                ),
                AdmissionCause::LifecycleExternalNudge { .. } => {
                    unreachable!("legacy WorkItem fixture cannot admit lifecycle nudges")
                }
                AdmissionCause::SettlementRecovery {
                    missing_activation_id,
                } => (
                    ActivationCause::SettlementRecovery {
                        activation_id: missing_activation_id.clone(),
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
                    ActivationOrigin::RuntimeRecovery,
                    ActivationTrust::RuntimeInstruction,
                ),
                AdmissionCause::InternalFollowup { message_id } => (
                    ActivationCause::InternalFollowup {
                        message_id: message_id.clone(),
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
                    ActivationOrigin::System,
                    ActivationTrust::RuntimeInstruction,
                ),
            };
            let command = AdmitActivationCommand {
                authority_id: format!("authority-{activation_id}"),
                activation: AgentActivation {
                    id: activation_id.clone(),
                    agent_id: "agent-1".into(),
                    state: ActivationLifecycleState::Admitted,
                    cause: typed_cause,
                    binding,
                    priority: ActivationPriority::Normal,
                    preemption: PreemptionPolicy::NonPreemptive,
                    source_revision: None,
                    idempotency_key: format!("activation-{activation_id}"),
                    provenance: ActivationProvenance {
                        origin,
                        trust,
                        source_id: "legacy-test-adapter".into(),
                        correlation_id: None,
                        causation_id: None,
                    },
                },
                expected_scheduling_generation: *expected_generation,
                expected_dispatch_revision: *expected_dispatch_revision,
            };
            reduce_command(snapshot, &ProtocolCommand::AdmitActivation(command)).outcome
        }
        Event::Settle {
            activation_id,
            settlement: Settlement::Missing,
        } => {
            reduce_command(
                snapshot,
                &ProtocolCommand::RecordMissingSettlement(MissingSettlementRecord {
                    id: format!("missing-{activation_id}"),
                    activation_id: activation_id.clone(),
                    created_at: "2026-07-20T00:00:00Z".into(),
                }),
            )
            .outcome
        }
        Event::Settle {
            activation_id,
            settlement,
        } => {
            let admitted_generation = snapshot.activations[activation_id].admitted_generation;
            let (disposition, agent_dispatch) = match settlement {
                Settlement::Continue => (
                    ActivationDisposition::WorkContinues,
                    AgentDispatchDisposition::Open,
                ),
                Settlement::Yield => (
                    ActivationDisposition::WorkYielded {
                        target_work_item_id: None,
                        continuation_id: None,
                        expected_target_generation: None,
                    },
                    AgentDispatchDisposition::Open,
                ),
                Settlement::TargetedYield { continuation } => (
                    ActivationDisposition::WorkYielded {
                        target_work_item_id: Some(continuation.target_work_item_id.clone()),
                        continuation_id: Some(continuation.continuation_id.clone()),
                        expected_target_generation: Some(continuation.target_generation),
                    },
                    AgentDispatchDisposition::Open,
                ),
                Settlement::Wait {
                    wait,
                    mode,
                    legacy_wait_id,
                } => {
                    let wait = WaitIdentity {
                        id: wait.id.clone(),
                        generation: if *legacy_wait_id {
                            admitted_generation + 1
                        } else {
                            wait.generation
                        },
                    };
                    (
                        ActivationDisposition::WorkWaits { wait: wait.clone() },
                        match mode {
                            model::WaitMode::AwaitThis => {
                                AgentDispatchDisposition::Awaiting { wait }
                            }
                            model::WaitMode::AcceptScheduling => AgentDispatchDisposition::Open,
                        },
                    )
                }
                Settlement::Complete { continuation } => (
                    ActivationDisposition::WorkCompleted {
                        continuation: continuation.clone(),
                    },
                    AgentDispatchDisposition::Open,
                ),
                Settlement::Missing => unreachable!(),
            };
            let completion = matches!(disposition, ActivationDisposition::WorkCompleted { .. });
            reduce_command(
                snapshot,
                &ProtocolCommand::SettleActivation(SettleActivationCommand {
                    settlement: ActivationSettlement {
                        id: format!("settlement-{activation_id}-{admitted_generation}"),
                        activation_id: activation_id.clone(),
                        turn_terminal: completion.then(|| {
                            format!("turn-terminal-{activation_id}-{admitted_generation}")
                        }),
                        disposition,
                        agent_dispatch,
                        operator_delivery: completion
                            .then(|| format!("brief-{activation_id}-{admitted_generation}")),
                        evidence: completion
                            .then(|| {
                                format!("completion-report:{activation_id}:{admitted_generation}")
                            })
                            .into_iter()
                            .collect(),
                        created_at: "2026-07-20T00:00:00Z".into(),
                    },
                }),
            )
            .outcome
        }
        Event::TriggerWait {
            wait_id,
            wait_generation,
            trigger_id,
            trigger_generation,
        } => {
            reduce_command(
                snapshot,
                &ProtocolCommand::TriggerWait(model::TriggerWaitCommand {
                    wait_id: wait_id.clone(),
                    wait_generation: *wait_generation,
                    trigger_id: trigger_id.clone(),
                    trigger_generation: *trigger_generation,
                }),
            )
            .outcome
        }
        _ => reduce(snapshot, event),
    }
}

#[test]
fn historical_scenarios_replay_to_one_explicit_state() {
    for fixture in fixtures() {
        let mut snapshot = fixture.initial;
        assert_invariants(&snapshot)
            .unwrap_or_else(|error| panic!("{}: initial invariant failed: {error}", fixture.name));
        let mut decisions = Vec::new();
        for input in &fixture.events {
            let first = apply_fixture_input(&snapshot, input);
            let second = apply_fixture_input(&snapshot, input);
            assert_eq!(
                first, second,
                "{}: reducer must be deterministic for {input:?}",
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
            snapshot.activation_admissions, fixture.expected.activation_admissions,
            "{}: activation admissions",
            fixture.name
        );
        assert_eq!(
            snapshot.settlements, fixture.expected.settlements,
            "{}: settlements",
            fixture.name
        );
        assert_eq!(
            snapshot.missing_settlements, fixture.expected.missing_settlements,
            "{}: missing settlements",
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
                .into_keys()
                .collect::<Vec<_>>(),
            fixture.expected.continuation_admissions,
            "{}: continuation admissions",
            fixture.name
        );
    }
}

#[test]
fn serialized_snapshot_preserves_canonical_activation_replay_fence() {
    let fixture = fixtures()
        .into_iter()
        .find(|fixture| fixture.name == "serialized_snapshot_fences_settled_activation")
        .expect("fixture");
    let first = apply_fixture_input(&fixture.initial, &fixture.events[0]);
    let encoded = serde_json::to_vec(&first.snapshot).expect("serialize");
    let reloaded: Snapshot = serde_json::from_slice(&encoded).expect("deserialize");
    let original_command = reloaded.activation_admissions["a1"].clone();
    let replay = reduce_command(
        &reloaded,
        &ProtocolCommand::AdmitActivation(original_command.clone()),
    );
    assert_eq!(replay.outcome.decision, Decision::DuplicateIgnored);
    assert_eq!(replay.outcome.snapshot, reloaded);

    let mut conflicting_command = original_command;
    conflicting_command.expected_scheduling_generation = 2;
    conflicting_command.activation.cause = ActivationCause::WorkItemRunnable {
        work_item_id: "w1".into(),
        scheduling_generation: 2,
    };
    let conflict = reduce_command(
        &reloaded,
        &ProtocolCommand::AdmitActivation(conflicting_command),
    );
    assert_eq!(conflict.outcome.decision, Decision::Rejected);
    assert_eq!(conflict.outcome.snapshot, reloaded);
    assert_eq!(
        conflict.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::IdentityConflict
    );
    assert_eq!(
        conflict.outcome.diagnostics,
        ["activation_id_command_conflict"]
    );
}

#[test]
fn legacy_wait_shapes_replay_with_explicit_generation_fences() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let legacy_event: Event = serde_json::from_value(serde_json::json!({
        "kind": "settle",
        "activation_id": "a1",
        "settlement": {
            "kind": "wait",
            "wait_id": "wait-1",
            "mode": "await_this"
        }
    }))
    .expect("deserialize legacy wait event");
    let encoded_event = serde_json::to_value(&legacy_event).expect("serialize legacy wait event");
    assert_eq!(
        encoded_event["settlement"]["wait_id"],
        serde_json::json!("wait-1")
    );
    assert!(encoded_event["settlement"].get("wait").is_none());
    let reloaded_event: Event =
        serde_json::from_value(encoded_event).expect("reload legacy wait event");
    assert_eq!(reloaded_event, legacy_event);

    let waiting = apply_event(&admitted.outcome.snapshot, &reloaded_event);
    assert_eq!(
        waiting,
        apply_event(&admitted.outcome.snapshot, &legacy_event),
        "legacy wait event replay must survive a serialization round trip"
    );
    assert_eq!(
        waiting.snapshot.dispatch,
        AgentDispatchState::Awaiting {
            wait: WaitIdentity {
                id: "wait-1".into(),
                generation: 2,
            },
        }
    );

    let mut encoded = serde_json::to_value(&waiting.snapshot).expect("serialize snapshot");
    encoded["dispatch"] = serde_json::json!({
        "kind": "awaiting",
        "wait_id": "wait-1"
    });
    let reloaded: Snapshot =
        serde_json::from_value(encoded).expect("deserialize legacy awaiting snapshot");
    assert_eq!(reloaded, waiting.snapshot);
}

#[test]
fn explicit_zero_wait_generations_are_not_upgraded_as_legacy_shapes() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let settlement = typed_settlement(
        "s1",
        "a1",
        ActivationDisposition::WorkWaits {
            wait: WaitIdentity {
                id: "wait-1".into(),
                generation: 2,
            },
        },
    );
    let mut settlement = settlement;
    settlement.agent_dispatch = AgentDispatchDisposition::Awaiting {
        wait: WaitIdentity {
            id: "wait-1".into(),
            generation: 2,
        },
    };
    let waiting = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand { settlement }),
    );
    assert_eq!(waiting.outcome.decision, Decision::Settled);

    let mut encoded = serde_json::to_value(&waiting.outcome.snapshot).expect("serialize snapshot");
    encoded["dispatch"]["wait"]["generation"] = serde_json::json!(0);
    encoded["settlements"]["s1"]["disposition"]["wait"]["generation"] = serde_json::json!(0);
    encoded["settlements"]["s1"]["agent_dispatch"]["wait"]["generation"] = serde_json::json!(0);
    let rejected = serde_json::from_value::<Snapshot>(encoded);
    assert!(rejected.is_err());

    let explicit_zero_event: Event = serde_json::from_value(serde_json::json!({
        "kind": "settle",
        "activation_id": "a1",
        "settlement": {
            "kind": "wait",
            "wait": {
                "id": "wait-1",
                "generation": 0
            },
            "mode": "await_this"
        }
    }))
    .expect("deserialize explicit typed zero wait event");
    let rejected = apply_event(&admitted.outcome.snapshot, &explicit_zero_event);
    assert_eq!(rejected.decision, Decision::Rejected);
    assert_eq!(rejected.diagnostics, ["wait_generation_required"]);
}

#[test]
fn legacy_dispatch_requires_an_authoritative_snapshot_wait_generation() {
    let standalone = serde_json::from_value::<AgentDispatchState>(serde_json::json!({
        "kind": "awaiting",
        "wait_id": "wait-1"
    }));
    assert!(standalone.is_err());

    let snapshot = minimal_snapshot(1);
    let mut encoded = serde_json::to_value(snapshot).expect("serialize snapshot");
    encoded["dispatch"] = serde_json::json!({
        "kind": "awaiting",
        "wait_id": "wait-1"
    });
    let reloaded = serde_json::from_value::<Snapshot>(encoded);
    assert!(reloaded.is_err());
}

#[test]
fn legacy_admit_and_settle_replay_through_the_public_migration_boundary() {
    let snapshot = minimal_snapshot(1);
    let admit_event = Event::Admit {
        activation_id: "a1".into(),
        owner: SchedulerOwner::WorkItem {
            work_item_id: "w1".into(),
        },
        expected_generation: 1,
        expected_dispatch_revision: 0,
        cause: AdmissionCause::Scheduling,
    };
    assert_eq!(
        reduce(&snapshot, &admit_event).diagnostics,
        ["typed_protocol_command_required"]
    );

    let admit_context = model::LegacyEventMigrationContext {
        record_id: "legacy-admit-1".into(),
        agent_id: "agent-1".into(),
        source_id: "legacy-event-log".into(),
        recorded_at: "2026-07-20T00:00:00Z".into(),
        admission_provenance: Some(ActivationProvenance {
            origin: ActivationOrigin::System,
            trust: ActivationTrust::RuntimeInstruction,
            source_id: "legacy-event-log".into(),
            correlation_id: None,
            causation_id: None,
        }),
        completion_report: None,
    };
    let admitted = model::migrate_legacy_event(&snapshot, &admit_event, &admit_context)
        .expect("legacy admission migration");
    assert_eq!(admitted.outcome.outcome.decision, Decision::Admitted);
    assert_invariants(&admitted.outcome.outcome.snapshot).expect("migrated admission is canonical");

    let replay = model::migrate_legacy_event(
        &admitted.outcome.outcome.snapshot,
        &admit_event,
        &admit_context,
    )
    .expect("legacy admission replay");
    assert_eq!(replay.outcome.outcome.decision, Decision::DuplicateIgnored);
    assert_eq!(
        replay.outcome.outcome.snapshot,
        admitted.outcome.outcome.snapshot
    );

    let settle_event = Event::Settle {
        activation_id: "a1".into(),
        settlement: Settlement::Continue,
    };
    let settled = model::migrate_legacy_event(
        &admitted.outcome.outcome.snapshot,
        &settle_event,
        &model::LegacyEventMigrationContext {
            record_id: "legacy-settle-1".into(),
            agent_id: "agent-1".into(),
            source_id: "legacy-event-log".into(),
            recorded_at: "2026-07-20T00:00:01Z".into(),
            admission_provenance: None,
            completion_report: None,
        },
    )
    .expect("legacy settlement migration");
    assert_eq!(settled.outcome.outcome.decision, Decision::Settled);
    assert_invariants(&settled.outcome.outcome.snapshot).expect("migrated settlement is canonical");
}

#[test]
fn legacy_wait_resume_migration_preserves_original_callback_provenance() {
    let admitted = apply_event(
        &minimal_snapshot(1),
        &Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let waiting = apply_event(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Wait {
                wait: WaitIdentity {
                    id: "wait-1".into(),
                    generation: 2,
                },
                mode: model::WaitMode::AwaitThis,
                legacy_wait_id: false,
            },
        },
    );
    let triggered = apply_event(
        &waiting.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 2,
            trigger_id: "callback-1".into(),
            trigger_generation: 1,
        },
    );
    let resume = Event::Admit {
        activation_id: "a2".into(),
        owner: SchedulerOwner::WorkItem {
            work_item_id: "w1".into(),
        },
        expected_generation: 2,
        expected_dispatch_revision: 1,
        cause: AdmissionCause::WaitResume {
            wait_id: "wait-1".into(),
            wait_generation: 2,
            trigger_id: "callback-1".into(),
            trigger_generation: 1,
        },
    };
    let provenance = ActivationProvenance {
        origin: ActivationOrigin::Callback,
        trust: ActivationTrust::IntegrationSignal,
        source_id: "callback-source".into(),
        correlation_id: Some("callback-correlation".into()),
        causation_id: Some("callback-cause".into()),
    };
    let context = model::LegacyEventMigrationContext {
        record_id: "legacy-resume-1".into(),
        agent_id: "agent-1".into(),
        source_id: "callback-source".into(),
        recorded_at: "2026-07-20T00:00:01Z".into(),
        admission_provenance: Some(provenance.clone()),
        completion_report: None,
    };

    let migrated = model::migrate_legacy_event(&triggered.snapshot, &resume, &context)
        .expect("legacy wait resume migration");
    assert_eq!(migrated.outcome.outcome.decision, Decision::Admitted);
    let ProtocolCommand::AdmitActivation(command) = migrated.command else {
        panic!("legacy wait resume must migrate to typed admission");
    };
    assert_eq!(command.activation.provenance, provenance);
    assert_eq!(
        migrated.outcome.outcome.snapshot.activation_admissions["a2"]
            .activation
            .provenance,
        provenance
    );
    assert_invariants(&migrated.outcome.outcome.snapshot)
        .expect("migrated callback provenance must remain canonical");

    let mut missing = context;
    missing.admission_provenance = None;
    let error = model::migrate_legacy_event(&triggered.snapshot, &resume, &missing)
        .expect_err("legacy admission without provenance must be rejected");
    assert_eq!(error.kind, ProtocolConflictKind::InvalidCommand);
    assert_eq!(error.code, "legacy_admission_provenance_required");
}

#[test]
fn snapshot_without_canonical_protocol_facts_is_rejected() {
    let snapshot = minimal_snapshot(1);
    let mut encoded = serde_json::to_value(snapshot).expect("serialize snapshot");
    encoded
        .as_object_mut()
        .expect("snapshot object")
        .remove("activation_admissions");
    let error = serde_json::from_value::<Snapshot>(encoded).expect_err("reject legacy snapshot");
    assert!(error
        .to_string()
        .contains("snapshot is missing canonical activation admissions"));
}

#[test]
fn wait_settlement_generation_mismatch_is_classified_as_stale_generation() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let snapshot = admitted.outcome.snapshot;
    let rejected = reduce_command(
        &snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement(
                "s1",
                "a1",
                ActivationDisposition::WorkWaits {
                    wait: WaitIdentity {
                        id: "wait-1".into(),
                        generation: 3,
                    },
                },
            ),
        }),
    );

    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(rejected.outcome.snapshot, snapshot);
    assert_eq!(
        rejected.conflict.expect("typed conflict"),
        model::ProtocolConflict {
            kind: ProtocolConflictKind::StaleGeneration,
            code: "wait_settlement_generation_mismatch".into(),
        }
    );
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
                wait: WaitIdentity { id: "wait-1".into(), generation: scheduling_generation },
            };
        }
        let event = Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem { work_item_id: "w1".into() },
            expected_generation,
            expected_dispatch_revision: snapshot.dispatch_revision,
            cause: model::AdmissionCause::Scheduling,
        };
        prop_assert_eq!(apply_event(&snapshot, &event), apply_event(&snapshot, &event));
    }

    #[test]
    fn stale_generation_never_acquires_the_single_activation_slot(
        scheduling_generation in 1_u64..100,
        delta in 1_u64..100,
    ) {
        let snapshot = minimal_snapshot(scheduling_generation);
        let event = Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem { work_item_id: "w1".into() },
            expected_generation: scheduling_generation.saturating_add(delta),
            expected_dispatch_revision: 0,
            cause: model::AdmissionCause::Scheduling,
        };
        let outcome = apply_event(&snapshot, &event);
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
            wait: WaitIdentity {
                id: "wait-1".into(),
                generation: 1,
            },
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
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
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
                let outcome = apply_event(&snapshot, event);
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
    let first_activation = apply_event(
        &minimal_snapshot(1),
        &Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    assert_eq!(first_activation.decision, Decision::Admitted);
    let first_wait = apply_event(
        &first_activation.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Wait {
                wait: WaitIdentity {
                    id: "wait-1".into(),
                    generation: 2,
                },
                mode: model::WaitMode::AwaitThis,
                legacy_wait_id: false,
            },
        },
    );
    assert_eq!(first_wait.decision, Decision::Settled);
    assert_eq!(first_wait.snapshot.waits["wait-1"].current_generation, 2);

    let first_trigger = apply_event(
        &first_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 2,
            trigger_id: "trigger-2".into(),
            trigger_generation: 1,
        },
    );
    assert_eq!(first_trigger.decision, Decision::WaitTriggered);
    let resumed = apply_event(
        &first_trigger.snapshot,
        &Event::Admit {
            activation_id: "a2".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
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
    let reused_wait = apply_event(
        &resumed.snapshot,
        &Event::Settle {
            activation_id: "a2".into(),
            settlement: Settlement::Wait {
                wait: WaitIdentity {
                    id: "wait-1".into(),
                    generation: 3,
                },
                mode: model::WaitMode::AwaitThis,
                legacy_wait_id: false,
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

    let stale_trigger = apply_event(
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

    let triggered = apply_event(
        &reused_wait.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 3,
            trigger_id: "trigger-3".into(),
            trigger_generation: 1,
        },
    );
    assert_eq!(triggered.decision, Decision::WaitTriggered);

    let stale_resume = apply_event(
        &triggered.snapshot,
        &Event::Admit {
            activation_id: "a-stale".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
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

    let current_resume = apply_event(
        &triggered.snapshot,
        &Event::Admit {
            activation_id: "a3".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
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
    let command = typed_admission("a1", "key-1", 4);
    let snapshot = minimal_snapshot(4);
    let mut snapshot = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command))
        .outcome
        .snapshot;
    snapshot
        .work
        .get_mut("w1")
        .expect("work")
        .scheduling_generation = 5;

    let outcome = apply_event(
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
    let admitted = apply_event(
        &minimal_snapshot(4),
        &Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 4,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let metadata_updated = apply_event(
        &admitted.snapshot,
        &Event::UpdateMetadata {
            work_item_id: "w1".into(),
            expected_metadata_revision: 4,
        },
    );
    assert_eq!(metadata_updated.decision, Decision::MetadataUpdated);

    let settled = apply_event(
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
    let admitted = apply_event(
        &minimal_snapshot(7),
        &Event::Admit {
            activation_id: "a-missing".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 7,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = apply_event(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a-missing".into(),
            settlement: Settlement::Missing,
        },
    );
    assert_eq!(missing.decision, Decision::SettlementMissing);
    assert_invariants(&missing.snapshot).expect("missing settlement must be canonical");

    let recovery = apply_event(
        &missing.snapshot,
        &Event::Admit {
            activation_id: "a-recovery".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 7,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::SettlementRecovery {
                missing_activation_id: "a-missing".into(),
            },
        },
    );
    assert_eq!(recovery.decision, Decision::Admitted);
    assert_invariants(&recovery.snapshot).expect("recovery activation must be canonical");

    let settled = apply_event(
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
fn wait_resume_missing_settlement_preserves_consumed_wait_for_recovery() {
    let admitted = apply_event(
        &minimal_snapshot(1),
        &Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let waiting = apply_event(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Wait {
                wait: WaitIdentity {
                    id: "wait-1".into(),
                    generation: 2,
                },
                mode: model::WaitMode::AwaitThis,
                legacy_wait_id: false,
            },
        },
    );
    let triggered = apply_event(
        &waiting.snapshot,
        &Event::TriggerWait {
            wait_id: "wait-1".into(),
            wait_generation: 2,
            trigger_id: "trigger-1".into(),
            trigger_generation: 1,
        },
    );
    let resumed = apply_event(
        &triggered.snapshot,
        &Event::Admit {
            activation_id: "a2".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 2,
            expected_dispatch_revision: 1,
            cause: AdmissionCause::WaitResume {
                wait_id: "wait-1".into(),
                wait_generation: 2,
                trigger_id: "trigger-1".into(),
                trigger_generation: 1,
            },
        },
    );
    assert_eq!(resumed.decision, Decision::Admitted);
    assert_eq!(
        resumed.snapshot.waits["wait-1"].generations[&2].state,
        WaitState::Consumed
    );
    assert_invariants(&resumed.snapshot)
        .expect("wait-resume admission must supersede the prior wait settlement projection");

    let missing = apply_event(
        &resumed.snapshot,
        &Event::Settle {
            activation_id: "a2".into(),
            settlement: Settlement::Missing,
        },
    );
    assert_eq!(missing.decision, Decision::SettlementMissing);
    assert_eq!(
        missing.snapshot.waits["wait-1"].generations[&2].state,
        WaitState::Consumed
    );
    assert_invariants(&missing.snapshot)
        .expect("missing wait-resume settlement must preserve recoverable consumed wait");

    let recovery = apply_event(
        &missing.snapshot,
        &Event::Admit {
            activation_id: "a3".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 2,
            expected_dispatch_revision: 2,
            cause: AdmissionCause::SettlementRecovery {
                missing_activation_id: "a2".into(),
            },
        },
    );
    assert_eq!(recovery.decision, Decision::Admitted);
    assert_invariants(&recovery.snapshot)
        .expect("recovery activation must retain the original consumed wait");

    let settled = apply_event(
        &recovery.snapshot,
        &Event::Settle {
            activation_id: "a3".into(),
            settlement: Settlement::Continue,
        },
    );
    assert_eq!(settled.decision, Decision::Settled);
    assert_eq!(
        settled.snapshot.waits["wait-1"].generations[&2].state,
        WaitState::Resolved
    );
    assert_eq!(settled.snapshot.work["w1"].scheduling_generation, 3);
    assert_eq!(settled.snapshot.work["w1"].status, WorkStatus::Runnable);
    assert_invariants(&settled.snapshot)
        .expect("recovered wait-resume settlement must resolve consumed wait");
}

#[test]
fn failed_settlement_recovery_enters_typed_hold_and_cannot_retry() {
    let admitted = apply_event(
        &minimal_snapshot(11),
        &Event::Admit {
            activation_id: "a-missing".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 11,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = apply_event(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a-missing".into(),
            settlement: Settlement::Missing,
        },
    );
    let recovery = apply_event(
        &missing.snapshot,
        &Event::Admit {
            activation_id: "a-recovery".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 11,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::SettlementRecovery {
                missing_activation_id: "a-missing".into(),
            },
        },
    );

    let held = apply_event(
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
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into()
            },
            admitted_generation: 11,
            state: ActivationState::SettlementMissing,
            recovery_for: Some("a-missing".into()),
        }
    );
    assert_invariants(&held.snapshot).expect("failed recovery must preserve typed hold invariants");

    let retry = apply_event(
        &held.snapshot,
        &Event::Admit {
            activation_id: "a-retry".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
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

    let stale = apply_event(
        &snapshot,
        &Event::Admit {
            activation_id: "a-stale-dispatch".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
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
fn settlement_recovery_rejects_a_current_revision_with_an_awaiting_reservation() {
    let mut base = minimal_snapshot(1);
    base.work.insert(
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

    let admitted_missing = apply_event(
        &base,
        &Event::Admit {
            activation_id: "a-missing".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = apply_event(
        &admitted_missing.snapshot,
        &Event::Settle {
            activation_id: "a-missing".into(),
            settlement: Settlement::Missing,
        },
    );

    let admitted_wait = apply_event(
        &base,
        &Event::Admit {
            activation_id: "a-wait".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w2".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let waiting = apply_event(
        &admitted_wait.snapshot,
        &Event::Settle {
            activation_id: "a-wait".into(),
            settlement: Settlement::Wait {
                wait: WaitIdentity {
                    id: "wait-w2".into(),
                    generation: 2,
                },
                mode: model::WaitMode::AwaitThis,
                legacy_wait_id: false,
            },
        },
    );

    let mut combined = missing.snapshot;
    combined
        .work
        .insert("w2".into(), waiting.snapshot.work["w2"].clone());
    combined.waits = waiting.snapshot.waits.clone();
    combined.dispatch = waiting.snapshot.dispatch.clone();
    combined.dispatch_revision = waiting.snapshot.dispatch_revision;
    combined.activation_admissions.insert(
        "a-wait".into(),
        waiting.snapshot.activation_admissions["a-wait"].clone(),
    );
    combined.activations.insert(
        "a-wait".into(),
        waiting.snapshot.activations["a-wait"].clone(),
    );
    combined.settlements.insert(
        "settlement-a-wait-1".into(),
        waiting.snapshot.settlements["settlement-a-wait-1"].clone(),
    );
    combined.admitted_generations.insert("work:w2:1".into());

    let mut recovery = typed_admission("a-recovery", "key-recovery", 1);
    recovery.expected_dispatch_revision = combined.dispatch_revision;
    recovery.activation.cause = ActivationCause::SettlementRecovery {
        activation_id: "a-missing".into(),
    };
    recovery.activation.provenance.origin = ActivationOrigin::RuntimeRecovery;
    assert_invariants(&combined)
        .expect("missing settlement and another work item's wait reservation are canonical");

    let rejected = reduce_command(&combined, &ProtocolCommand::AdmitActivation(recovery));
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.outcome.diagnostics,
        ["settlement_recovery_lane_reserved"]
    );
    assert_eq!(rejected.outcome.snapshot, combined);
}

#[test]
fn pending_settlement_recovery_blocks_ordinary_work_admission() {
    let admitted = apply_event(
        &minimal_snapshot(5),
        &Event::Admit {
            activation_id: "a-missing".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 5,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = apply_event(
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

    let ordinary = apply_event(
        &snapshot,
        &Event::Admit {
            activation_id: "a-w2".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w2".into(),
            },
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
fn targeted_yield_is_fenced_and_target_completion_restores_source_once() {
    let mut snapshot = minimal_snapshot(1);
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
    let admitted = apply_event(
        &snapshot,
        &Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let continuation = YieldContinuationRecord {
        continuation_id: "yield-1".into(),
        source_work_item_id: "w1".into(),
        source_generation: 1,
        target_work_item_id: "w2".into(),
        target_generation: 1,
    };
    let yielded = apply_event(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::TargetedYield {
                continuation: continuation.clone(),
            },
        },
    );
    assert_eq!(
        yielded.snapshot.work["w1"].status,
        WorkStatus::Yielded {
            continuation: continuation.clone()
        }
    );
    assert_eq!(yielded.snapshot.focus.as_deref(), Some("w2"));

    let target = apply_event(
        &yielded.snapshot,
        &Event::Admit {
            activation_id: "a2".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w2".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: yielded.snapshot.dispatch_revision,
            cause: AdmissionCause::Scheduling,
        },
    );
    let completed = apply_event(
        &target.snapshot,
        &Event::Settle {
            activation_id: "a2".into(),
            settlement: Settlement::Complete { continuation: None },
        },
    );
    assert_eq!(completed.snapshot.work["w1"].status, WorkStatus::Runnable);
    assert_eq!(completed.snapshot.focus.as_deref(), Some("w1"));
    assert_invariants(&completed.snapshot).expect("target completion restores source exactly once");

    let legacy: Settlement =
        serde_json::from_value(serde_json::json!({"kind": "yield"})).expect("legacy yield");
    assert_eq!(legacy, Settlement::Yield);
    assert_eq!(
        serde_json::to_value(legacy).expect("serialize legacy yield"),
        serde_json::json!({"kind": "yield"})
    );
}

#[test]
fn typed_commands_retain_admission_identity_and_replay_deterministically() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(
        &snapshot,
        &ProtocolCommand::AdmitActivation(command.clone()),
    );
    assert_eq!(admitted.outcome.decision, Decision::Admitted);
    assert_eq!(
        admitted.outcome.snapshot.activation_admissions["a1"],
        command
    );
    assert_invariants(&admitted.outcome.snapshot).expect("typed admission must be canonical");

    let replay = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::AdmitActivation(command.clone()),
    );
    assert_eq!(replay.outcome.decision, Decision::DuplicateIgnored);
    assert_eq!(replay.outcome.snapshot, admitted.outcome.snapshot);

    let conflict = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::AdmitActivation(typed_admission("a2", "key-1", 1)),
    );
    assert_eq!(conflict.outcome.decision, Decision::Rejected);
    assert_eq!(
        conflict.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::IdempotencyConflict
    );
    assert_eq!(
        conflict.outcome.diagnostics,
        ["activation_idempotency_key_conflict"]
    );
}

#[test]
fn typed_admission_rejects_provenance_spoofing() {
    let mut command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    command.activation.provenance.trust = ActivationTrust::ExternalEvidence;
    let rejected = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::InvalidCommand
    );
    assert_eq!(
        rejected.outcome.diagnostics,
        ["activation_provenance_authority_mismatch"]
    );
    assert_eq!(rejected.outcome.snapshot, snapshot);
}

#[test]
fn typed_settlement_has_one_canonical_identity_and_idempotent_replay() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let settlement = typed_settlement("s1", "a1", ActivationDisposition::WorkContinues);
    let settled = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: settlement.clone(),
        }),
    );
    assert_eq!(settled.outcome.decision, Decision::Settled);
    assert_eq!(settled.outcome.snapshot.settlements["s1"], settlement);
    assert_invariants(&settled.outcome.snapshot).expect("typed settlement must be canonical");

    let replay = reduce_command(
        &settled.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: settlement.clone(),
        }),
    );
    assert_eq!(replay.outcome.decision, Decision::DuplicateIgnored);
    assert_eq!(replay.outcome.snapshot, settled.outcome.snapshot);

    let conflicting = reduce_command(
        &settled.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement("s2", "a1", ActivationDisposition::WorkContinues),
        }),
    );
    assert_eq!(conflicting.outcome.decision, Decision::Rejected);
    assert_eq!(
        conflicting.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StateConflict
    );
    assert_eq!(
        conflicting.outcome.diagnostics,
        ["activation_terminal_settlement_already_recorded"]
    );
}

#[test]
fn lifecycle_settlement_can_atomically_adopt_work_item_wait() {
    let mut snapshot = minimal_snapshot(1);
    snapshot.focus = None;
    snapshot.work.clear();
    let activation_id = "activation:message:msg-1";
    let admission = AdmitActivationCommand {
        authority_id: "authority-lifecycle".into(),
        activation: AgentActivation {
            id: activation_id.into(),
            agent_id: "agent-1".into(),
            state: ActivationLifecycleState::Admitted,
            cause: ActivationCause::LifecycleExternalNudge {
                message_id: "msg-1".into(),
            },
            binding: ActivationBinding::Lifecycle {
                agent_id: "agent-1".into(),
            },
            priority: ActivationPriority::Normal,
            preemption: PreemptionPolicy::NonPreemptive,
            source_revision: None,
            idempotency_key: "lifecycle-msg-1".into(),
            provenance: ActivationProvenance {
                origin: ActivationOrigin::System,
                trust: ActivationTrust::RuntimeInstruction,
                source_id: "scheduler".into(),
                correlation_id: None,
                causation_id: None,
            },
        },
        expected_scheduling_generation: 1,
        expected_dispatch_revision: 0,
    };
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(admission));
    let settled = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: ActivationSettlement {
                id: "settlement:message:msg-1".into(),
                activation_id: activation_id.into(),
                turn_terminal: Some("turn-1".into()),
                disposition: ActivationDisposition::WorkContinues,
                agent_dispatch: AgentDispatchDisposition::Open,
                operator_delivery: None,
                evidence: vec!["message:msg-1".into()],
                created_at: "2026-07-30T00:00:00Z".into(),
            },
        }),
    );
    let command = ProtocolCommand::AdoptActivationWorkState(AdoptActivationWorkStateCommand {
        source_activation_id: activation_id.into(),
        source_message_id: "msg-1".into(),
        source_turn_id: "turn-1".into(),
        source_admitted_generation: 1,
        work_item_id: "work-1".into(),
        source_work_item_revision: 2,
        wait: LegacyWaitAdoption {
            wait_id: "wait-1".into(),
            generation: 2,
            owner_work_item_id: "work-1".into(),
            source_updated_at: "2026-07-30T00:00:01Z".into(),
        },
        focus: true,
        reserve_dispatch: true,
    });

    let adopted = reduce_command(&settled.outcome.snapshot, &command);
    assert_eq!(adopted.outcome.decision, Decision::LegacyWorkStateAdopted);
    assert_eq!(adopted.outcome.snapshot.focus.as_deref(), Some("work-1"));
    assert_eq!(
        adopted.outcome.snapshot.dispatch,
        AgentDispatchState::Awaiting {
            wait: WaitIdentity {
                id: "wait-1".into(),
                generation: 2,
            },
        }
    );
    assert_invariants(&adopted.outcome.snapshot).unwrap();

    let replay = reduce_command(&adopted.outcome.snapshot, &command);
    assert_eq!(replay.outcome.decision, Decision::DuplicateIgnored);

    let mut rearm_command = command.clone();
    let ProtocolCommand::AdoptActivationWorkState(rearm) = &mut rearm_command else {
        unreachable!("fixture is an activation adoption command");
    };
    rearm.source_work_item_revision = 3;
    rearm.wait.wait_id = "wait-2".into();
    rearm.wait.generation = 3;
    rearm.wait.source_updated_at = "2026-07-30T00:00:02Z".into();

    let mut stale_command = rearm_command.clone();
    let ProtocolCommand::AdoptActivationWorkState(stale) = &mut stale_command else {
        unreachable!("fixture is an activation adoption command");
    };
    stale.source_work_item_revision = 1;
    stale.wait.generation = 1;
    let stale = reduce_command(&adopted.outcome.snapshot, &stale_command);
    assert_eq!(stale.outcome.decision, Decision::Rejected);
    assert_eq!(
        stale.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StaleRevision
    );

    let mut terminal_snapshot = adopted.outcome.snapshot.clone();
    terminal_snapshot.focus = None;
    terminal_snapshot.dispatch = AgentDispatchState::Open;
    terminal_snapshot.work.get_mut("work-1").unwrap().status = WorkStatus::Terminal;
    terminal_snapshot
        .work
        .get_mut("work-1")
        .unwrap()
        .scheduling_generation = 3;
    terminal_snapshot
        .waits
        .get_mut("wait-1")
        .unwrap()
        .generations
        .get_mut(&2)
        .unwrap()
        .state = WaitState::Resolved;
    assert_invariants(&terminal_snapshot).unwrap();
    let mut terminal_command = rearm_command.clone();
    let ProtocolCommand::AdoptActivationWorkState(terminal) = &mut terminal_command else {
        unreachable!("fixture is an activation adoption command");
    };
    terminal.source_work_item_revision = 4;
    terminal.wait.generation = 4;
    let terminal = reduce_command(&terminal_snapshot, &terminal_command);
    assert_eq!(terminal.outcome.decision, Decision::Rejected);
    assert_eq!(
        terminal.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StateConflict
    );

    let mut stale_generation_snapshot = adopted.outcome.snapshot.clone();
    let work = stale_generation_snapshot.work.get_mut("work-1").unwrap();
    work.scheduling_generation = 5;
    let wait = stale_generation_snapshot.waits.get_mut("wait-1").unwrap();
    wait.generations.get_mut(&2).unwrap().state = WaitState::Resolved;
    wait.current_generation = 5;
    wait.generations.insert(
        5,
        WaitGenerationRecord {
            owner: SchedulerOwner::WorkItem {
                work_item_id: "work-1".into(),
            },
            state: WaitState::Active,
            trigger: None,
            consuming_activation_id: None,
        },
    );
    stale_generation_snapshot.dispatch = AgentDispatchState::Awaiting {
        wait: WaitIdentity {
            id: "wait-1".into(),
            generation: 5,
        },
    };
    assert_invariants(&stale_generation_snapshot).unwrap();
    let stale_generation = reduce_command(&stale_generation_snapshot, &rearm_command);
    assert_eq!(stale_generation.outcome.decision, Decision::Rejected);
    assert_eq!(
        stale_generation.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StaleGeneration
    );

    let mut unrelated_dispatch_snapshot = adopted.outcome.snapshot.clone();
    unrelated_dispatch_snapshot.waits.insert(
        "wait-lifecycle".into(),
        WaitRecord {
            current_generation: 1,
            generations: BTreeMap::from([(
                1,
                WaitGenerationRecord {
                    owner: SchedulerOwner::AgentLifecycle {
                        agent_id: "agent-1".into(),
                    },
                    state: WaitState::Active,
                    trigger: None,
                    consuming_activation_id: None,
                },
            )]),
        },
    );
    unrelated_dispatch_snapshot.dispatch = AgentDispatchState::Awaiting {
        wait: WaitIdentity {
            id: "wait-lifecycle".into(),
            generation: 1,
        },
    };
    assert_invariants(&unrelated_dispatch_snapshot).unwrap();
    let unrelated_dispatch = reduce_command(&unrelated_dispatch_snapshot, &rearm_command);
    assert_eq!(unrelated_dispatch.outcome.decision, Decision::Rejected);
    assert_eq!(
        unrelated_dispatch.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StateConflict
    );

    let mut ownership_conflict_snapshot = adopted.outcome.snapshot.clone();
    ownership_conflict_snapshot.work.insert(
        "work-2".into(),
        WorkDemand {
            metadata_revision: 5,
            scheduling_generation: 5,
            status: WorkStatus::Runnable,
            capabilities: BTreeSet::new(),
            locks: BTreeSet::new(),
            locality: "runtime".into(),
            cost_class: "default".into(),
        },
    );
    ownership_conflict_snapshot.waits.insert(
        "wait-2".into(),
        WaitRecord {
            current_generation: 1,
            generations: BTreeMap::from([(
                1,
                WaitGenerationRecord {
                    owner: SchedulerOwner::WorkItem {
                        work_item_id: "work-2".into(),
                    },
                    state: WaitState::Resolved,
                    trigger: None,
                    consuming_activation_id: None,
                },
            )]),
        },
    );
    assert_invariants(&ownership_conflict_snapshot).unwrap();
    let ownership_conflict = reduce_command(&ownership_conflict_snapshot, &rearm_command);
    assert_eq!(ownership_conflict.outcome.decision, Decision::Rejected);
    assert_eq!(
        ownership_conflict.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::IdentityConflict
    );

    let mut reusable_wait_snapshot = adopted.outcome.snapshot.clone();
    reusable_wait_snapshot.waits.insert(
        "wait-2".into(),
        WaitRecord {
            current_generation: 1,
            generations: BTreeMap::from([(
                1,
                WaitGenerationRecord {
                    owner: SchedulerOwner::WorkItem {
                        work_item_id: "work-1".into(),
                    },
                    state: WaitState::Resolved,
                    trigger: None,
                    consuming_activation_id: None,
                },
            )]),
        },
    );
    assert_invariants(&reusable_wait_snapshot).unwrap();
    let reusable_wait = reduce_command(&reusable_wait_snapshot, &rearm_command);
    assert_eq!(
        reusable_wait.outcome.decision,
        Decision::LegacyWorkStateAdopted
    );
    assert_eq!(
        reusable_wait.outcome.snapshot.waits["wait-2"].current_generation,
        3
    );
    assert_eq!(
        reusable_wait.outcome.snapshot.waits["wait-2"].generations[&1].state,
        WaitState::Resolved
    );
    assert_invariants(&reusable_wait.outcome.snapshot).unwrap();

    let rearmed = reduce_command(&adopted.outcome.snapshot, &rearm_command);
    assert_eq!(rearmed.outcome.decision, Decision::LegacyWorkStateAdopted);
    assert_eq!(
        rearmed.outcome.snapshot.work["work-1"].status,
        WorkStatus::Waiting {
            wait_id: "wait-2".into(),
        }
    );
    assert_eq!(
        rearmed.outcome.snapshot.waits["wait-1"].generations[&2].state,
        WaitState::Resolved
    );
    assert_eq!(
        rearmed.outcome.snapshot.waits["wait-2"].generations[&3].state,
        WaitState::Active
    );
    assert_eq!(
        rearmed.outcome.snapshot.dispatch,
        AgentDispatchState::Awaiting {
            wait: WaitIdentity {
                id: "wait-2".into(),
                generation: 3,
            },
        }
    );
    assert_invariants(&rearmed.outcome.snapshot).unwrap();

    let mut reused_wait_command = rearm_command.clone();
    let ProtocolCommand::AdoptActivationWorkState(reused_wait) = &mut reused_wait_command else {
        unreachable!("fixture is an activation adoption command");
    };
    reused_wait.source_work_item_revision = 4;
    reused_wait.wait.generation = 4;
    reused_wait.wait.source_updated_at = "2026-07-30T00:00:03Z".into();
    let reused_wait = reduce_command(&rearmed.outcome.snapshot, &reused_wait_command);
    assert_eq!(
        reused_wait.outcome.decision,
        Decision::LegacyWorkStateAdopted
    );
    let wait = &reused_wait.outcome.snapshot.waits["wait-2"];
    assert_eq!(wait.current_generation, 4);
    assert_eq!(wait.generations[&3].state, WaitState::Resolved);
    assert_eq!(wait.generations[&4].state, WaitState::Active);
    assert_invariants(&reused_wait.outcome.snapshot).unwrap();

    let mut release_dispatch_command = rearm_command;
    let ProtocolCommand::AdoptActivationWorkState(release_dispatch) = &mut release_dispatch_command
    else {
        unreachable!("fixture is an activation adoption command");
    };
    release_dispatch.source_work_item_revision = 5;
    release_dispatch.wait.wait_id = "wait-3".into();
    release_dispatch.wait.generation = 5;
    release_dispatch.wait.source_updated_at = "2026-07-30T00:00:04Z".into();
    release_dispatch.reserve_dispatch = false;
    let released = reduce_command(&reused_wait.outcome.snapshot, &release_dispatch_command);
    assert_eq!(released.outcome.decision, Decision::LegacyWorkStateAdopted);
    assert_eq!(released.outcome.snapshot.dispatch, AgentDispatchState::Open);
    assert_eq!(
        released.outcome.snapshot.dispatch_revision,
        reused_wait.outcome.snapshot.dispatch_revision + 1
    );
    assert_invariants(&released.outcome.snapshot).unwrap();
}

#[test]
fn work_demand_registration_is_typed_idempotent_and_conflict_safe() {
    let snapshot = minimal_snapshot(1);
    let demand = WorkDemand {
        metadata_revision: 3,
        scheduling_generation: 1,
        status: WorkStatus::Runnable,
        capabilities: BTreeSet::from(["workspace_write".into()]),
        locks: BTreeSet::from(["workspace:holon".into()]),
        locality: "workspace:holon".into(),
        cost_class: "standard".into(),
    };
    let command = ProtocolCommand::RegisterWorkDemand(RegisterWorkDemandCommand {
        work_item_id: "work-b".into(),
        demand: demand.clone(),
    });

    let registered = reduce_command(&snapshot, &command);
    assert_eq!(registered.outcome.decision, Decision::WorkDemandRegistered);
    assert_eq!(registered.outcome.snapshot.work["work-b"], demand);
    assert_invariants(&registered.outcome.snapshot).expect("registered demand must be canonical");

    let replay = reduce_command(&registered.outcome.snapshot, &command);
    assert_eq!(replay.outcome.decision, Decision::DuplicateIgnored);
    assert_eq!(replay.outcome.snapshot, registered.outcome.snapshot);

    let conflicting = reduce_command(
        &registered.outcome.snapshot,
        &ProtocolCommand::RegisterWorkDemand(RegisterWorkDemandCommand {
            work_item_id: "work-b".into(),
            demand: WorkDemand {
                metadata_revision: 4,
                ..registered.outcome.snapshot.work["work-b"].clone()
            },
        }),
    );
    assert_eq!(conflicting.outcome.decision, Decision::Rejected);
    assert_eq!(
        conflicting.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::IdentityConflict
    );
    assert_eq!(
        conflicting.outcome.diagnostics,
        ["work_demand_registration_conflict"]
    );
    assert_eq!(conflicting.outcome.snapshot, registered.outcome.snapshot);
}

#[test]
fn admission_authority_id_is_unique_and_cannot_have_an_alias() {
    let admission = typed_admission("a1", "key-1", 1);
    let admitted = reduce_command(
        &minimal_snapshot(1),
        &ProtocolCommand::AdmitActivation(admission.clone()),
    );
    assert_eq!(admitted.outcome.decision, Decision::Admitted);
    assert_invariants(&admitted.outcome.snapshot).expect("admission must be canonical");

    let replay = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::AdmitActivation(admission.clone()),
    );
    assert_eq!(replay.outcome.decision, Decision::DuplicateIgnored);
    assert_eq!(replay.outcome.snapshot, admitted.outcome.snapshot);

    let mut duplicate_identity = typed_admission("a2", "key-2", 2);
    duplicate_identity.authority_id = admission.authority_id;
    let rejected = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::AdmitActivation(duplicate_identity),
    );
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::AuthorityConflict
    );
    assert_eq!(
        rejected.outcome.diagnostics,
        ["activation_authority_id_command_conflict"]
    );
    assert_eq!(rejected.outcome.snapshot, admitted.outcome.snapshot);

    let mut aliased = admitted.outcome.snapshot;
    let mut alias = typed_admission("a2", "key-2", 2);
    alias.authority_id = aliased.activation_admissions["a1"].authority_id.clone();
    aliased.activation_admissions.insert("a2".into(), alias);
    aliased.activations.insert(
        "a2".into(),
        ActivationRecord {
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            admitted_generation: 2,
            state: ActivationState::Settled,
            recovery_for: None,
        },
    );
    aliased.admitted_generations.insert("work:w1:2".into());
    assert_eq!(
        assert_invariants(&aliased),
        Err("canonical activation admissions reuse authority identity".into())
    );
    let encoded = serde_json::to_value(aliased).expect("serialize invalid snapshot");
    let error = serde_json::from_value::<Snapshot>(encoded)
        .expect_err("reject aliased admission authority id");
    assert!(error
        .to_string()
        .contains("canonical activation admissions reuse authority identity"));
}

#[test]
fn typed_wait_settlement_rejects_empty_wait_identity_without_mutation() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let snapshot = admitted.outcome.snapshot;
    let rejected = reduce_command(
        &snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement(
                "s1",
                "a1",
                ActivationDisposition::WorkWaits {
                    wait: WaitIdentity {
                        id: String::new(),
                        generation: 2,
                    },
                },
            ),
        }),
    );
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::InvalidCommand
    );
    assert_eq!(rejected.outcome.diagnostics, ["wait_identity_required"]);
    assert_eq!(rejected.outcome.snapshot, snapshot);
}

#[test]
fn already_admitted_generation_is_classified_as_duplicate() {
    let mut snapshot = minimal_snapshot(1);
    snapshot.admitted_generations.insert("work:w1:1".into());
    let command = typed_admission("a2", "key-2", 1);
    let rejected = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::Duplicate
    );
    assert_eq!(
        rejected.outcome.diagnostics,
        ["scheduling_generation_already_admitted"]
    );
}

#[test]
fn wait_history_rejects_future_generations_and_rearm_never_overwrites_them() {
    let command = typed_admission("a1", "key-1", 3);
    let snapshot = minimal_snapshot(3);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let mut snapshot = admitted.outcome.snapshot;
    snapshot.waits.insert(
        "wait-1".into(),
        WaitRecord {
            current_generation: 2,
            generations: BTreeMap::from([
                (
                    2,
                    WaitGenerationRecord {
                        owner: SchedulerOwner::WorkItem {
                            work_item_id: "w1".into(),
                        },
                        state: WaitState::Resolved,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                ),
                (
                    4,
                    WaitGenerationRecord {
                        owner: SchedulerOwner::WorkItem {
                            work_item_id: "w1".into(),
                        },
                        state: WaitState::Resolved,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                ),
            ]),
        },
    );

    assert_eq!(
        assert_invariants(&snapshot),
        Err("wait wait-1 has future generation 4 beyond current generation 2".into())
    );

    let rejected = reduce_command(
        &snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement(
                "s1",
                "a1",
                ActivationDisposition::WorkWaits {
                    wait: WaitIdentity {
                        id: "wait-1".into(),
                        generation: 4,
                    },
                },
            ),
        }),
    );
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.outcome.diagnostics,
        ["wait_generation_already_exists"]
    );
    assert_eq!(rejected.outcome.snapshot, snapshot);

    let mut farther_future = snapshot;
    let wait = farther_future.waits.get_mut("wait-1").expect("wait");
    wait.generations.remove(&4);
    wait.generations.insert(
        5,
        WaitGenerationRecord {
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            state: WaitState::Resolved,
            trigger: None,
            consuming_activation_id: None,
        },
    );
    let rejected = reduce_command(
        &farther_future,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement(
                "s1",
                "a1",
                ActivationDisposition::WorkWaits {
                    wait: WaitIdentity {
                        id: "wait-1".into(),
                        generation: 4,
                    },
                },
            ),
        }),
    );
    assert_eq!(rejected.outcome.decision, Decision::Rejected);
    assert_eq!(
        rejected.outcome.diagnostics,
        ["wait_history_has_future_generation"]
    );
    assert_eq!(rejected.outcome.snapshot, farther_future);
}

#[test]
fn canonical_admissions_rebuild_exact_unique_reservation_fences() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let snapshot = admitted.outcome.snapshot;

    let mut missing_fence = snapshot.clone();
    missing_fence.admitted_generations.clear();
    assert!(assert_invariants(&missing_fence)
        .expect_err("missing fence must violate canonical admissions")
        .starts_with("canonical admission fences disagree with activation admissions"));

    let mut fabricated_fence = snapshot.clone();
    fabricated_fence
        .admitted_generations
        .insert("work:w1:99".into());
    assert!(assert_invariants(&fabricated_fence)
        .expect_err("fabricated fence must violate canonical admissions")
        .starts_with("canonical admission fences disagree with activation admissions"));

    let mut duplicate = snapshot;
    let duplicate_command = typed_admission("a2", "key-2", 1);
    duplicate
        .activation_admissions
        .insert("a2".into(), duplicate_command);
    duplicate.activations.insert(
        "a2".into(),
        ActivationRecord {
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            admitted_generation: 1,
            state: ActivationState::Settled,
            recovery_for: None,
        },
    );
    assert_eq!(
        assert_invariants(&duplicate),
        Err("canonical activation admissions reuse an admission fence".into())
    );
}

#[test]
fn canonical_admission_recovery_rejects_future_dispatch_fences_and_alias_authority_ids() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let snapshot = admitted.outcome.snapshot;

    let mut future_dispatch_fence = snapshot.clone();
    future_dispatch_fence
        .activation_admissions
        .get_mut("a1")
        .expect("admission")
        .expected_dispatch_revision = 99;
    assert_eq!(
        assert_invariants(&future_dispatch_fence),
        Err("canonical activation admission record is invalid".into())
    );

    let mut aliased_authority = snapshot;
    let mut alias = typed_admission("a2", "key-2", 2);
    alias.authority_id = aliased_authority.activation_admissions["a1"]
        .authority_id
        .clone();
    aliased_authority
        .activation_admissions
        .insert("a2".into(), alias);
    aliased_authority.activations.insert(
        "a2".into(),
        ActivationRecord {
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            admitted_generation: 2,
            state: ActivationState::Settled,
            recovery_for: None,
        },
    );
    aliased_authority
        .admitted_generations
        .insert("work:w1:2".into());
    assert_eq!(
        assert_invariants(&aliased_authority),
        Err("canonical activation admissions reuse authority identity".into())
    );
}

#[test]
fn every_activation_requires_a_canonical_admission_and_terminal_record() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));

    let mut running_without_admission = admitted.outcome.snapshot.clone();
    running_without_admission.activation_admissions.clear();
    running_without_admission.admitted_generations.clear();
    assert_eq!(
        assert_invariants(&running_without_admission),
        Err("canonical activation has no canonical admission".into())
    );

    let mut settled_without_records = running_without_admission;
    settled_without_records.slot = ActivationSlot::Idle;
    settled_without_records
        .activations
        .get_mut("a1")
        .expect("activation")
        .state = ActivationState::Settled;
    let work = settled_without_records.work.get_mut("w1").expect("work");
    work.scheduling_generation = 2;
    work.status = WorkStatus::Runnable;
    assert_eq!(
        assert_invariants(&settled_without_records),
        Err("canonical activation has no canonical admission".into())
    );
}

#[test]
fn completion_continuation_requires_a_runnable_caller() {
    let command = typed_admission("a1", "key-1", 1);
    let mut snapshot = minimal_snapshot(1);
    snapshot.work.insert(
        "caller".into(),
        WorkDemand {
            metadata_revision: 1,
            scheduling_generation: 1,
            status: WorkStatus::Waiting {
                wait_id: "caller-wait".into(),
            },
            capabilities: ["workspace_read".into()].into_iter().collect(),
            locks: BTreeSet::new(),
            locality: "workspace:holon".into(),
            cost_class: "standard".into(),
        },
    );
    snapshot.waits.insert(
        "caller-wait".into(),
        wait_record("caller", 1, WaitState::Active, None, None),
    );
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    assert_invariants(&admitted.outcome.snapshot).expect("waiting caller prestate is canonical");

    let completion = |snapshot: &Snapshot, settlement_id: &str| {
        reduce_command(
            snapshot,
            &ProtocolCommand::SettleActivation(SettleActivationCommand {
                settlement: typed_settlement(
                    settlement_id,
                    "a1",
                    ActivationDisposition::WorkCompleted {
                        continuation: Some(model::Continuation {
                            admission_id: format!("continuation-{settlement_id}"),
                            caller_work_item_id: "caller".into(),
                            expected_caller_generation: 1,
                        }),
                    },
                ),
            }),
        )
    };

    let waiting = completion(&admitted.outcome.snapshot, "s-waiting");
    assert_eq!(waiting.outcome.decision, Decision::Rejected);
    assert_eq!(
        waiting.outcome.diagnostics,
        ["continuation_caller_not_runnable"]
    );
    assert_eq!(
        waiting.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StateConflict
    );
    assert_eq!(waiting.outcome.snapshot, admitted.outcome.snapshot);

    let mut terminal_caller = admitted.outcome.snapshot;
    terminal_caller
        .work
        .get_mut("caller")
        .expect("caller")
        .status = WorkStatus::Terminal;
    terminal_caller.waits.clear();
    assert_invariants(&terminal_caller).expect("terminal caller prestate is canonical");
    let terminal = completion(&terminal_caller, "s-terminal");
    assert_eq!(terminal.outcome.decision, Decision::Rejected);
    assert_eq!(
        terminal.outcome.diagnostics,
        ["continuation_caller_not_runnable"]
    );
    assert_eq!(terminal.outcome.snapshot, terminal_caller);
}

#[test]
fn canonical_continuation_records_are_exact_and_preserve_the_caller_prestate_fence() {
    let command = typed_admission("a1", "key-1", 1);
    let mut snapshot = minimal_snapshot(1);
    snapshot.work.insert(
        "caller".into(),
        WorkDemand {
            metadata_revision: 4,
            scheduling_generation: 4,
            status: WorkStatus::Runnable,
            capabilities: ["workspace_read".into()].into_iter().collect(),
            locks: BTreeSet::new(),
            locality: "workspace:holon".into(),
            cost_class: "standard".into(),
        },
    );
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let completed = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement(
                "s1",
                "a1",
                ActivationDisposition::WorkCompleted {
                    continuation: Some(model::Continuation {
                        admission_id: "continuation-1".into(),
                        caller_work_item_id: "caller".into(),
                        expected_caller_generation: 4,
                    }),
                },
            ),
        }),
    );
    let snapshot = completed.outcome.snapshot;
    assert_invariants(&snapshot).expect("canonical continuation");

    let mut orphan = snapshot.clone();
    let mut orphan_record = orphan.continuation_admissions["continuation-1"].clone();
    orphan_record.admission_id = "orphan".into();
    orphan
        .continuation_admissions
        .insert("orphan".into(), orphan_record);
    assert_eq!(
        assert_invariants(&orphan),
        Err("canonical continuation admissions disagree with completion settlements".into())
    );

    let mut wrong_prestate = snapshot;
    wrong_prestate
        .continuation_admissions
        .get_mut("continuation-1")
        .expect("continuation")
        .expected_caller_status = WorkStatus::Terminal;
    assert_eq!(
        assert_invariants(&wrong_prestate),
        Err("canonical continuation admissions disagree with completion settlements".into())
    );
}

#[test]
fn canonical_typed_records_must_match_authoritative_activation_and_wait_facts() {
    let command = typed_admission("a1", "key-1", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));

    let mut mismatched_admission = admitted.outcome.snapshot.clone();
    mismatched_admission
        .activations
        .get_mut("a1")
        .expect("activation")
        .admitted_generation = 2;
    assert!(assert_invariants(&mismatched_admission).is_err());

    let settled = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement("s1", "a1", ActivationDisposition::WorkContinues),
        }),
    );
    let settled_snapshot = settled.outcome.snapshot;
    let mut mismatched_settlement = settled_snapshot.clone();
    mismatched_settlement
        .settlements
        .get_mut("s1")
        .expect("settlement")
        .disposition = ActivationDisposition::WorkWaits {
        wait: WaitIdentity {
            id: "wait-never-created".into(),
            generation: 2,
        },
    };
    assert!(assert_invariants(&mismatched_settlement).is_err());

    let mut runnable_settlement_with_terminal_work = settled_snapshot;
    runnable_settlement_with_terminal_work
        .work
        .get_mut("w1")
        .expect("work")
        .status = WorkStatus::Terminal;
    assert!(assert_invariants(&runnable_settlement_with_terminal_work).is_err());

    let command = typed_admission("a2", "key-2", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let completed = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement(
                "s2",
                "a2",
                ActivationDisposition::WorkCompleted { continuation: None },
            ),
        }),
    );
    let mut completed_with_runnable_work = completed.outcome.snapshot;
    completed_with_runnable_work
        .work
        .get_mut("w1")
        .expect("work")
        .status = WorkStatus::Runnable;
    assert!(assert_invariants(&completed_with_runnable_work).is_err());

    let command = typed_admission("a3", "key-3", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let mut wait_settlement = typed_settlement(
        "s3",
        "a3",
        ActivationDisposition::WorkWaits {
            wait: WaitIdentity {
                id: "wait-1".into(),
                generation: 2,
            },
        },
    );
    wait_settlement.agent_dispatch = AgentDispatchDisposition::Awaiting {
        wait: WaitIdentity {
            id: "wait-1".into(),
            generation: 2,
        },
    };
    let waiting = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: wait_settlement,
        }),
    );
    let waiting_snapshot = waiting.outcome.snapshot;
    let mut waiting_with_open_lane = waiting_snapshot.clone();
    waiting_with_open_lane.dispatch = AgentDispatchState::Open;
    assert!(assert_invariants(&waiting_with_open_lane).is_err());

    let mut waiting_with_resolved_current_generation = waiting_snapshot;
    waiting_with_resolved_current_generation.dispatch = AgentDispatchState::Open;
    waiting_with_resolved_current_generation
        .waits
        .get_mut("wait-1")
        .expect("wait")
        .generations
        .get_mut(&2)
        .expect("current wait generation")
        .state = WaitState::Resolved;
    assert!(assert_invariants(&waiting_with_resolved_current_generation).is_err());

    let command = typed_admission("a4", "key-4", 1);
    let snapshot = minimal_snapshot(1);
    let admitted = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    let missing = reduce_command(
        &admitted.outcome.snapshot,
        &ProtocolCommand::RecordMissingSettlement(MissingSettlementRecord {
            id: "missing-a4".into(),
            activation_id: "a4".into(),
            created_at: "2026-07-20T00:00:00Z".into(),
        }),
    );
    assert_invariants(&missing.outcome.snapshot).expect("canonical missing settlement");
    let mut missing_without_record = missing.outcome.snapshot;
    missing_without_record.missing_settlements.clear();
    assert!(assert_invariants(&missing_without_record).is_err());
}

#[test]
fn reducer_rejections_have_explicit_stable_conflict_kinds() {
    let no_running = reduce_command(
        &minimal_snapshot(1),
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: typed_settlement("s1", "a1", ActivationDisposition::WorkContinues),
        }),
    );
    assert_eq!(
        no_running.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::AuthorityConflict
    );
    assert_eq!(
        no_running.outcome.diagnostics,
        ["activation_has_no_canonical_admission"]
    );

    let no_canonical_admission = reduce_command(
        &minimal_snapshot(1),
        &ProtocolCommand::RecordMissingSettlement(MissingSettlementRecord {
            id: "missing-a1".into(),
            activation_id: "a1".into(),
            created_at: "2026-07-20T00:00:00Z".into(),
        }),
    );
    assert_eq!(
        no_canonical_admission
            .conflict
            .expect("typed conflict")
            .kind,
        ProtocolConflictKind::AuthorityConflict
    );
    assert_eq!(
        no_canonical_admission.outcome.diagnostics,
        ["activation_has_no_canonical_admission"]
    );
    assert_eq!(no_canonical_admission.outcome.snapshot, minimal_snapshot(1));

    let admitted = apply_event(
        &minimal_snapshot(1),
        &Event::Admit {
            activation_id: "a1".into(),
            owner: SchedulerOwner::WorkItem {
                work_item_id: "w1".into(),
            },
            expected_generation: 1,
            expected_dispatch_revision: 0,
            cause: AdmissionCause::Scheduling,
        },
    );
    let missing = apply_event(
        &admitted.snapshot,
        &Event::Settle {
            activation_id: "a1".into(),
            settlement: Settlement::Missing,
        },
    );
    let command = typed_admission("a2", "key-2", 1);
    let snapshot = missing.snapshot;
    let rejected = reduce_command(&snapshot, &ProtocolCommand::AdmitActivation(command));
    assert_eq!(
        rejected.conflict.expect("typed conflict").kind,
        ProtocolConflictKind::StateConflict
    );
    assert_eq!(
        rejected.outcome.diagnostics,
        ["settlement_recovery_pending"]
    );
}

fn typed_admission(
    activation_id: &str,
    idempotency_key: &str,
    generation: u64,
) -> AdmitActivationCommand {
    AdmitActivationCommand {
        authority_id: format!("authority-{activation_id}"),
        activation: AgentActivation {
            id: activation_id.into(),
            agent_id: "agent-1".into(),
            state: ActivationLifecycleState::Admitted,
            cause: ActivationCause::WorkItemRunnable {
                work_item_id: "w1".into(),
                scheduling_generation: generation,
            },
            binding: ActivationBinding::WorkItem {
                work_item_id: "w1".into(),
            },
            priority: ActivationPriority::Normal,
            preemption: PreemptionPolicy::NonPreemptive,
            source_revision: Some(7),
            idempotency_key: idempotency_key.into(),
            provenance: ActivationProvenance {
                origin: ActivationOrigin::System,
                trust: ActivationTrust::RuntimeInstruction,
                source_id: "scheduler".into(),
                correlation_id: Some("corr-1".into()),
                causation_id: Some("cause-1".into()),
            },
        },
        expected_scheduling_generation: generation,
        expected_dispatch_revision: 0,
    }
}

fn typed_settlement(
    settlement_id: &str,
    activation_id: &str,
    disposition: ActivationDisposition,
) -> ActivationSettlement {
    ActivationSettlement {
        id: settlement_id.into(),
        activation_id: activation_id.into(),
        turn_terminal: Some("turn-terminal-1".into()),
        disposition,
        agent_dispatch: AgentDispatchDisposition::Open,
        operator_delivery: Some("brief-1".into()),
        evidence: vec!["trace-1".into()],
        created_at: "2026-07-20T00:00:00Z".into(),
    }
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
                owner: SchedulerOwner::WorkItem {
                    work_item_id: owner_work_item_id.into(),
                },
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
        activation_admissions: BTreeMap::new(),
        settlements: BTreeMap::new(),
        missing_settlements: BTreeMap::new(),
        admitted_generations: Default::default(),
        continuation_admissions: Default::default(),
        activation_inputs: Default::default(),
    }
}
