use std::collections::{BTreeMap, BTreeSet};

use holon::domain::scheduler_protocol::{
    assert_invariants, reduce_command, ActivationBinding, ActivationCause, ActivationDisposition,
    ActivationLifecycleState, ActivationOrigin, ActivationPriority, ActivationProvenance,
    ActivationSettlement, ActivationSlot, ActivationTrust, AdmitActivationCommand, AgentActivation,
    AgentDispatchDisposition, AgentDispatchState, Decision, PreemptionPolicy, ProtocolCommand,
    SchedulerOwner, SettleActivationCommand, Snapshot, TriggerWaitCommand, WaitGenerationRecord,
    WaitIdentity, WaitRecord, WaitState,
};

fn owner() -> SchedulerOwner {
    SchedulerOwner::AgentLifecycle {
        agent_id: "agent-a".into(),
    }
}

fn waiting_snapshot() -> Snapshot {
    let wait = WaitIdentity {
        id: "wait-a".into(),
        generation: 2,
    };
    Snapshot {
        slot: ActivationSlot::Idle,
        dispatch: AgentDispatchState::Awaiting { wait: wait.clone() },
        dispatch_revision: 0,
        focus: None,
        work: BTreeMap::new(),
        waits: BTreeMap::from([(
            wait.id.clone(),
            WaitRecord {
                current_generation: 2,
                generations: BTreeMap::from([(
                    2,
                    WaitGenerationRecord {
                        owner: owner(),
                        state: WaitState::Active,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                )]),
            },
        )]),
        activations: BTreeMap::new(),
        activation_admissions: BTreeMap::new(),
        settlements: BTreeMap::new(),
        missing_settlements: BTreeMap::new(),
        admitted_generations: BTreeSet::new(),
        continuation_admissions: BTreeMap::new(),
        activation_inputs: BTreeMap::new(),
    }
}

fn admission(id: &str, cause: ActivationCause) -> AdmitActivationCommand {
    AdmitActivationCommand {
        authority_id: format!("authority-{id}"),
        activation: AgentActivation {
            id: id.into(),
            agent_id: "agent-a".into(),
            state: ActivationLifecycleState::Admitted,
            cause,
            binding: ActivationBinding::Lifecycle {
                agent_id: "agent-a".into(),
            },
            priority: ActivationPriority::Next,
            preemption: PreemptionPolicy::AllowOperatorInterjection,
            source_revision: None,
            idempotency_key: format!("activation-key-{id}"),
            provenance: ActivationProvenance {
                origin: ActivationOrigin::Callback,
                trust: ActivationTrust::IntegrationSignal,
                source_id: format!("message-{id}"),
                correlation_id: None,
                causation_id: None,
            },
        },
        expected_scheduling_generation: 2,
        expected_dispatch_revision: 0,
    }
}

fn admit(snapshot: &Snapshot, command: AdmitActivationCommand) -> Snapshot {
    let admitted = reduce_command(snapshot, &ProtocolCommand::AdmitActivation(command));
    assert_eq!(admitted.outcome.decision, Decision::Admitted);
    admitted.outcome.snapshot
}

#[test]
fn generic_lifecycle_nudge_does_not_consume_wait() {
    let initial = waiting_snapshot();
    let admitted = admit(
        &initial,
        admission(
            "nudge",
            ActivationCause::LifecycleExternalNudge {
                message_id: "message-nudge".into(),
            },
        ),
    );
    let settled = reduce_command(
        &admitted,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: ActivationSettlement {
                id: "settlement-nudge".into(),
                activation_id: "nudge".into(),
                turn_terminal: Some("turn-nudge".into()),
                disposition: ActivationDisposition::WorkContinues,
                agent_dispatch: AgentDispatchDisposition::Open,
                operator_delivery: None,
                evidence: vec!["message:message-nudge".into()],
                created_at: "2026-07-27T00:00:00Z".into(),
            },
        }),
    );
    assert_eq!(settled.outcome.decision, Decision::Settled);
    assert_eq!(settled.outcome.snapshot.dispatch, initial.dispatch);
    assert_eq!(
        settled.outcome.snapshot.waits["wait-a"].generations[&2].state,
        WaitState::Active
    );
    assert_invariants(&settled.outcome.snapshot).unwrap();
}

#[test]
fn generic_lifecycle_nudge_rearm_resolves_previous_wait() {
    let admitted = admit(
        &waiting_snapshot(),
        admission(
            "nudge-rearm",
            ActivationCause::LifecycleExternalNudge {
                message_id: "message-nudge-rearm".into(),
            },
        ),
    );
    let next_wait = WaitIdentity {
        id: "wait-b".into(),
        generation: 3,
    };
    let settled = reduce_command(
        &admitted,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: ActivationSettlement {
                id: "settlement-nudge-rearm".into(),
                activation_id: "nudge-rearm".into(),
                turn_terminal: Some("turn-nudge-rearm".into()),
                disposition: ActivationDisposition::WorkWaits {
                    wait: next_wait.clone(),
                },
                agent_dispatch: AgentDispatchDisposition::Awaiting {
                    wait: next_wait.clone(),
                },
                operator_delivery: None,
                evidence: vec!["message:message-nudge-rearm".into()],
                created_at: "2026-07-27T00:00:00Z".into(),
            },
        }),
    );
    assert_eq!(settled.outcome.decision, Decision::Settled);
    assert_eq!(
        settled.outcome.snapshot.waits["wait-a"].generations[&2].state,
        WaitState::Resolved
    );
    assert_eq!(
        settled.outcome.snapshot.waits["wait-b"].generations[&3].state,
        WaitState::Active
    );
    assert_eq!(
        settled.outcome.snapshot.dispatch,
        AgentDispatchState::Awaiting { wait: next_wait }
    );
    assert_invariants(&settled.outcome.snapshot).unwrap();
}

#[test]
fn lifecycle_exact_resume_consumes_and_rearms_generation() {
    let triggered = reduce_command(
        &waiting_snapshot(),
        &ProtocolCommand::TriggerWait(TriggerWaitCommand {
            wait_id: "wait-a".into(),
            wait_generation: 2,
            trigger_id: "delivery-a".into(),
            trigger_generation: 7,
        }),
    );
    assert_eq!(triggered.outcome.decision, Decision::WaitTriggered);
    let admitted = admit(
        &triggered.outcome.snapshot,
        admission(
            "resume",
            ActivationCause::WaitResume {
                wait_id: "wait-a".into(),
                wait_generation: 2,
                trigger_id: "delivery-a".into(),
                trigger_generation: 7,
            },
        ),
    );
    assert_eq!(
        admitted.waits["wait-a"].generations[&2].state,
        WaitState::Consumed
    );
    let next_wait = WaitIdentity {
        id: "wait-a".into(),
        generation: 3,
    };
    let settled = reduce_command(
        &admitted,
        &ProtocolCommand::SettleActivation(SettleActivationCommand {
            settlement: ActivationSettlement {
                id: "settlement-resume".into(),
                activation_id: "resume".into(),
                turn_terminal: Some("turn-resume".into()),
                disposition: ActivationDisposition::WorkWaits {
                    wait: next_wait.clone(),
                },
                agent_dispatch: AgentDispatchDisposition::Awaiting {
                    wait: next_wait.clone(),
                },
                operator_delivery: None,
                evidence: vec!["message:message-resume".into()],
                created_at: "2026-07-27T00:00:00Z".into(),
            },
        }),
    );
    assert_eq!(settled.outcome.decision, Decision::Settled);
    assert_eq!(
        settled.outcome.snapshot.waits["wait-a"].current_generation,
        3
    );
    assert_eq!(
        settled.outcome.snapshot.waits["wait-a"].generations[&2].state,
        WaitState::Resolved
    );
    assert_eq!(
        settled.outcome.snapshot.waits["wait-a"].generations[&3].owner,
        owner()
    );
    assert_eq!(
        settled.outcome.snapshot.dispatch,
        AgentDispatchState::Awaiting { wait: next_wait }
    );
    assert_invariants(&settled.outcome.snapshot).unwrap();
}
