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
    pub state: WaitState,
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
    WaitResume { wait_id: String },
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
        Event::TriggerWait { wait_id } => trigger_wait(snapshot, wait_id),
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
        AdmissionCause::WaitResume { wait_id } => {
            let Some(wait) = snapshot.waits.get(wait_id) else {
                return rejected(snapshot, "unknown_wait");
            };
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
    };
    transitions.push(format!("slot:idle->running:{activation_id}"));
    Outcome {
        decision: Decision::Admitted,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn trigger_wait(snapshot: &Snapshot, wait_id: &str) -> Outcome {
    let Some(wait) = snapshot.waits.get(wait_id) else {
        return rejected(snapshot, "unknown_wait");
    };
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
    let mut next = snapshot.clone();
    let work = next
        .work
        .get_mut(work_item_id)
        .expect("running work item exists");
    work.revision = current_work.revision + 1;

    let mut transitions = vec![
        format!("activation:{activation_id}:settled"),
        format!(
            "work:{work_item_id}:revision:{}->{}",
            current_work.revision, work.revision
        ),
    ];

    match settlement {
        Settlement::Continue | Settlement::Yield => {
            work.status = WorkStatus::Runnable;
            next.dispatch = AgentDispatchState::Open;
            transitions.push(format!("work:{work_item_id}:runnable"));
        }
        Settlement::Wait { wait_id, mode } => {
            if next.waits.contains_key(wait_id) {
                return rejected(snapshot, "wait_id_already_exists");
            }
            work.status = WorkStatus::Waiting {
                wait_id: wait_id.clone(),
            };
            next.waits.insert(
                wait_id.clone(),
                WaitRecord {
                    owner_work_item_id: work_item_id.clone(),
                    state: WaitState::Active,
                },
            );
            next.dispatch = match mode {
                WaitMode::AwaitThis => AgentDispatchState::ReservedFor {
                    wait_id: wait_id.clone(),
                },
                WaitMode::AcceptScheduling => AgentDispatchState::Open,
            };
            transitions.push(format!("wait:{wait_id}:created:active"));
        }
        Settlement::Complete { continuation } => {
            work.status = WorkStatus::Terminal;
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
    } = &snapshot.slot
    {
        if snapshot.settled_activations.contains(activation_id) {
            return Err("settled activation still owns the slot".into());
        }
        if !snapshot.work.contains_key(work_item_id) {
            return Err("running activation references unknown work item".into());
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
