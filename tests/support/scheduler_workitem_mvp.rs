use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub slot: ActivationSlot,
    pub dispatch: AgentDispatchState,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub work: BTreeMap<String, WorkDemand>,
    #[serde(default)]
    pub waits: BTreeMap<String, WaitRecord>,
    #[serde(default)]
    pub admitted_revisions: BTreeSet<String>,
    #[serde(default)]
    pub settled_activations: BTreeSet<String>,
    #[serde(default)]
    pub continuation_admissions: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivationSlot {
    Idle,
    Running {
        activation_id: String,
        work_item_id: String,
        admitted_revision: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDispatchState {
    Open,
    ReservedFor { wait_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDemand {
    pub revision: u64,
    pub status: WorkStatus,
    pub capabilities: BTreeSet<String>,
    pub locks: BTreeSet<String>,
    pub locality: String,
    pub cost_class: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkStatus {
    Runnable,
    Waiting { wait_id: String },
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitRecord {
    pub owner_work_item_id: String,
    pub generation: u64,
    pub state: WaitState,
    #[serde(default)]
    pub resolved_generations: BTreeSet<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitState {
    Active,
    Triggered,
    Consumed,
    Resolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Admit {
        activation_id: String,
        work_item_id: String,
        expected_revision: u64,
        cause: AdmissionCause,
    },
    TriggerWait {
        wait_id: String,
        generation: u64,
    },
    OperatorIntervention {
        input_id: String,
    },
    Settle {
        activation_id: String,
        settlement: Settlement,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdmissionCause {
    Scheduling,
    WaitResume { wait_id: String, generation: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Settlement {
    Continue,
    Yield,
    Wait {
        wait_id: String,
        mode: WaitMode,
    },
    Complete {
        #[serde(default)]
        continuation: Option<Continuation>,
    },
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitMode {
    AwaitThis,
    AcceptScheduling,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Continuation {
    pub admission_id: String,
    pub caller_work_item_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub decision: Decision,
    pub transitions: Vec<String>,
    pub diagnostics: Vec<String>,
    pub snapshot: Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Admitted,
    Settled,
    WaitTriggered,
    DuplicateIgnored,
    OperatorIntervention,
    SettlementMissing,
    Rejected,
}

pub fn reduce(snapshot: &Snapshot, event: &Event) -> Outcome {
    match event {
        Event::Admit {
            activation_id,
            work_item_id,
            expected_revision,
            cause,
        } => admit(
            snapshot,
            activation_id,
            work_item_id,
            *expected_revision,
            cause,
        ),
        Event::TriggerWait {
            wait_id,
            generation,
        } => trigger_wait(snapshot, wait_id, *generation),
        Event::OperatorIntervention { input_id } => Outcome {
            decision: Decision::OperatorIntervention,
            transitions: Vec::new(),
            diagnostics: vec![format!("operator_intervention:{input_id}")],
            snapshot: snapshot.clone(),
        },
        Event::Settle {
            activation_id,
            settlement,
        } => settle(snapshot, activation_id, settlement),
    }
}

fn admit(
    snapshot: &Snapshot,
    activation_id: &str,
    work_item_id: &str,
    expected_revision: u64,
    cause: &AdmissionCause,
) -> Outcome {
    if snapshot.settled_activations.contains(activation_id) {
        return rejected(snapshot, "activation_already_settled");
    }
    if !matches!(snapshot.slot, ActivationSlot::Idle) {
        return rejected(snapshot, "activation_slot_not_idle");
    }

    let Some(work) = snapshot.work.get(work_item_id) else {
        return rejected(snapshot, "unknown_work_item");
    };
    if work.revision != expected_revision {
        return rejected(snapshot, "stale_scheduling_revision");
    }

    let admission_fence = format!("{work_item_id}:{expected_revision}");
    if snapshot.admitted_revisions.contains(&admission_fence) {
        return rejected(snapshot, "scheduling_revision_already_admitted");
    }

    let mut next = snapshot.clone();
    let mut transitions = Vec::new();
    match cause {
        AdmissionCause::Scheduling => {
            if !matches!(work.status, WorkStatus::Runnable) {
                return rejected(snapshot, "work_item_not_runnable");
            }
            if !matches!(snapshot.dispatch, AgentDispatchState::Open) {
                return rejected(snapshot, "agent_lane_reserved");
            }
        }
        AdmissionCause::WaitResume {
            wait_id,
            generation,
        } => {
            let Some(wait) = snapshot.waits.get(wait_id) else {
                return rejected(snapshot, "unknown_wait");
            };
            if wait.generation != *generation {
                return rejected(snapshot, "stale_wait_generation");
            }
            if wait.owner_work_item_id != work_item_id {
                return rejected(snapshot, "wait_owner_mismatch");
            }
            if wait.state != WaitState::Triggered {
                return rejected(snapshot, "wait_not_triggered");
            }
            if work.status
                != (WorkStatus::Waiting {
                    wait_id: wait_id.clone(),
                })
            {
                return rejected(snapshot, "work_item_not_waiting_for_wait");
            }
            if let AgentDispatchState::ReservedFor {
                wait_id: reserved_wait,
            } = &snapshot.dispatch
            {
                if reserved_wait != wait_id {
                    return rejected(snapshot, "agent_lane_reserved_for_other_wait");
                }
            }

            next.waits.get_mut(wait_id).expect("wait exists").state = WaitState::Consumed;
            transitions.push(format!("wait:{wait_id}:triggered->consumed"));
            if matches!(
                snapshot.dispatch,
                AgentDispatchState::ReservedFor { wait_id: ref reserved_wait }
                    if reserved_wait == wait_id
            ) {
                next.dispatch = AgentDispatchState::Open;
            }
        }
    }

    next.admitted_revisions.insert(admission_fence);
    next.slot = ActivationSlot::Running {
        activation_id: activation_id.to_string(),
        work_item_id: work_item_id.to_string(),
        admitted_revision: expected_revision,
    };
    transitions.push(format!("slot:idle->running:{activation_id}"));
    Outcome {
        decision: Decision::Admitted,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn trigger_wait(snapshot: &Snapshot, wait_id: &str, generation: u64) -> Outcome {
    let Some(wait) = snapshot.waits.get(wait_id) else {
        return rejected(snapshot, "unknown_wait");
    };
    if wait.generation != generation {
        return rejected(snapshot, "stale_wait_generation");
    }
    if wait.state != WaitState::Active {
        return Outcome {
            decision: Decision::DuplicateIgnored,
            transitions: Vec::new(),
            diagnostics: vec![format!("wait_not_active:{wait_id}:{:?}", wait.state)],
            snapshot: snapshot.clone(),
        };
    }

    let mut next = snapshot.clone();
    next.waits.get_mut(wait_id).expect("wait exists").state = WaitState::Triggered;
    Outcome {
        decision: Decision::WaitTriggered,
        transitions: vec![format!("wait:{wait_id}:active->triggered")],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn settle(snapshot: &Snapshot, activation_id: &str, settlement: &Settlement) -> Outcome {
    let ActivationSlot::Running {
        activation_id: running_activation_id,
        work_item_id,
        admitted_revision,
    } = &snapshot.slot
    else {
        return rejected(snapshot, "no_running_activation");
    };
    if running_activation_id != activation_id {
        return rejected(snapshot, "activation_id_mismatch");
    }

    if matches!(settlement, Settlement::Missing) {
        return Outcome {
            decision: Decision::SettlementMissing,
            transitions: Vec::new(),
            diagnostics: vec![format!("settlement_missing:{activation_id}")],
            snapshot: snapshot.clone(),
        };
    }

    let Some(current_work) = snapshot.work.get(work_item_id) else {
        return rejected(snapshot, "running_work_item_missing");
    };
    if current_work.revision != *admitted_revision {
        return rejected(snapshot, "stale_activation_revision");
    }
    let consumed_wait_id = match &current_work.status {
        WorkStatus::Waiting { wait_id }
            if snapshot.waits.get(wait_id).is_some_and(|wait| {
                wait.owner_work_item_id == *work_item_id && wait.state == WaitState::Consumed
            }) =>
        {
            Some(wait_id.clone())
        }
        _ => None,
    };
    let mut next = snapshot.clone();
    let next_revision = current_work.revision + 1;
    next.work
        .get_mut(work_item_id)
        .expect("running work item exists")
        .revision = next_revision;

    let mut transitions = vec![
        format!("activation:{activation_id}:settled"),
        format!(
            "work:{work_item_id}:revision:{}->{}",
            current_work.revision, next_revision
        ),
    ];

    match settlement {
        Settlement::Continue | Settlement::Yield => {
            resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Runnable;
            next.dispatch = AgentDispatchState::Open;
            transitions.push(format!("work:{work_item_id}:runnable"));
        }
        Settlement::Wait { wait_id, mode } => {
            if let Some(existing_wait) = next.waits.get(wait_id) {
                if matches!(
                    existing_wait.state,
                    WaitState::Active | WaitState::Triggered
                ) {
                    return rejected(snapshot, "wait_id_still_active");
                }
                if existing_wait.owner_work_item_id != *work_item_id {
                    return rejected(snapshot, "wait_id_owner_mismatch");
                }
                if existing_wait.generation >= next_revision {
                    return rejected(snapshot, "wait_generation_not_advanced");
                }
                if existing_wait.state == WaitState::Consumed
                    && consumed_wait_id.as_deref() != Some(wait_id)
                {
                    return rejected(snapshot, "wait_id_consumed_by_other_activation");
                }
            }
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Waiting {
                wait_id: wait_id.clone(),
            };
            if consumed_wait_id.as_deref().is_some_and(|id| id != wait_id) {
                resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            }
            let previous_generation = next.waits.get(wait_id).map(|wait| wait.generation);
            if let Some(wait) = next.waits.get_mut(wait_id) {
                if wait.state == WaitState::Consumed {
                    wait.resolved_generations.insert(wait.generation);
                    transitions.push(format!(
                        "wait:{wait_id}:generation:{}:consumed->resolved",
                        wait.generation
                    ));
                } else if wait.state == WaitState::Resolved {
                    wait.resolved_generations.insert(wait.generation);
                }
                wait.generation = next_revision;
                wait.state = WaitState::Active;
            } else {
                next.waits.insert(
                    wait_id.clone(),
                    WaitRecord {
                        owner_work_item_id: work_item_id.clone(),
                        generation: next_revision,
                        state: WaitState::Active,
                        resolved_generations: BTreeSet::new(),
                    },
                );
            }
            next.dispatch = match mode {
                WaitMode::AwaitThis => AgentDispatchState::ReservedFor {
                    wait_id: wait_id.clone(),
                },
                WaitMode::AcceptScheduling => AgentDispatchState::Open,
            };
            transitions.push(match previous_generation {
                Some(generation) => {
                    format!("wait:{wait_id}:generation:{generation}->{next_revision}:active")
                }
                None => format!("wait:{wait_id}:created:active"),
            });
        }
        Settlement::Complete { continuation } => {
            resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Terminal;
            next.dispatch = AgentDispatchState::Open;
            transitions.push(format!("work:{work_item_id}:terminal"));
            if let Some(continuation) = continuation {
                if !next
                    .continuation_admissions
                    .insert(continuation.admission_id.clone())
                {
                    return rejected(snapshot, "continuation_already_admitted");
                }
                let Some(caller) = next.work.get_mut(&continuation.caller_work_item_id) else {
                    return rejected(snapshot, "continuation_caller_missing");
                };
                caller.revision += 1;
                caller.status = WorkStatus::Runnable;
                transitions.push(format!(
                    "continuation:{}:{}:runnable",
                    continuation.admission_id, continuation.caller_work_item_id
                ));
            }
        }
        Settlement::Missing => unreachable!("handled above"),
    }

    next.slot = ActivationSlot::Idle;
    next.settled_activations
        .insert(running_activation_id.clone());
    Outcome {
        decision: Decision::Settled,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn resolve_consumed_wait(
    snapshot: &mut Snapshot,
    wait_id: Option<&str>,
    transitions: &mut Vec<String>,
) {
    let Some(wait_id) = wait_id else {
        return;
    };
    let wait = snapshot
        .waits
        .get_mut(wait_id)
        .expect("consumed wait exists");
    wait.resolved_generations.insert(wait.generation);
    wait.state = WaitState::Resolved;
    transitions.push(format!(
        "wait:{wait_id}:generation:{}:consumed->resolved",
        wait.generation
    ));
}

fn rejected(snapshot: &Snapshot, diagnostic: &str) -> Outcome {
    Outcome {
        decision: Decision::Rejected,
        transitions: Vec::new(),
        diagnostics: vec![diagnostic.to_string()],
        snapshot: snapshot.clone(),
    }
}

pub fn assert_invariants(snapshot: &Snapshot) -> Result<(), String> {
    if let ActivationSlot::Running {
        activation_id,
        work_item_id,
        admitted_revision,
    } = &snapshot.slot
    {
        if snapshot.settled_activations.contains(activation_id) {
            return Err("settled activation still owns the slot".into());
        }
        let work = snapshot
            .work
            .get(work_item_id)
            .ok_or_else(|| "running activation references unknown work item".to_string())?;
        if work.revision != *admitted_revision {
            return Err("running activation revision fence does not match work item".into());
        }
    }

    if let AgentDispatchState::ReservedFor { wait_id } = &snapshot.dispatch {
        let wait = snapshot
            .waits
            .get(wait_id)
            .ok_or_else(|| "lane reservation references unknown wait".to_string())?;
        if !matches!(wait.state, WaitState::Active | WaitState::Triggered) {
            return Err("lane reservation references inactive wait".into());
        }
        let work = snapshot
            .work
            .get(&wait.owner_work_item_id)
            .ok_or_else(|| "reserved wait references unknown owner".to_string())?;
        if work.status
            != (WorkStatus::Waiting {
                wait_id: wait_id.clone(),
            })
        {
            return Err("reserved wait owner is not waiting for that wait".into());
        }
    }

    for (wait_id, wait) in &snapshot.waits {
        if matches!(wait.state, WaitState::Active | WaitState::Triggered) {
            let owner = snapshot
                .work
                .get(&wait.owner_work_item_id)
                .ok_or_else(|| format!("wait {wait_id} references unknown owner"))?;
            if wait.generation != owner.revision {
                return Err(format!(
                    "active wait {wait_id} generation does not match owner revision"
                ));
            }
            if owner.status
                != (WorkStatus::Waiting {
                    wait_id: wait_id.clone(),
                })
            {
                return Err(format!("active wait {wait_id} has non-waiting owner"));
            }
        }
    }

    Ok(())
}
