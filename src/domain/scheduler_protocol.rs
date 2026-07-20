//! Pure deterministic scheduler protocol kernel.
//!
//! This module is the production home of the executable Scheduler / WorkItem
//! baseline. It is intentionally storage-independent and has no call site in
//! the production scheduler while the legacy scheduler remains authoritative.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    pub slot: ActivationSlot,
    pub dispatch: AgentDispatchState,
    #[serde(default)]
    pub dispatch_revision: u64,
    #[serde(default)]
    pub focus: Option<String>,
    #[serde(default)]
    pub work: BTreeMap<String, WorkDemand>,
    #[serde(default)]
    pub waits: BTreeMap<String, WaitRecord>,
    #[serde(default)]
    pub activations: BTreeMap<String, ActivationRecord>,
    #[serde(default)]
    pub rollout: RolloutState,
    #[serde(default)]
    pub admitted_generations: BTreeSet<String>,
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
        admitted_generation: u64,
        #[serde(default)]
        recovery_for: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDispatchState {
    Open,
    Awaiting { wait_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDemand {
    pub metadata_revision: u64,
    pub scheduling_generation: u64,
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
    NeedsSettlement { activation_id: String },
    Paused { hold_id: String },
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationRecord {
    pub work_item_id: String,
    pub admitted_generation: u64,
    pub state: ActivationState,
    #[serde(default)]
    pub recovery_for: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationState {
    Running,
    Settled,
    SettlementMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitRecord {
    pub current_generation: u64,
    pub generations: BTreeMap<u64, WaitGenerationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitGenerationRecord {
    pub owner_work_item_id: String,
    pub state: WaitState,
    #[serde(default)]
    pub trigger: Option<WaitTrigger>,
    #[serde(default)]
    pub consuming_activation_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitTrigger {
    pub trigger_id: String,
    pub trigger_generation: u64,
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
pub struct RolloutState {
    pub protocol_mode: ProtocolMode,
    pub config_revision: u64,
    #[serde(default)]
    pub latest_preflight_revision: u64,
    #[serde(default)]
    pub preflights: BTreeMap<u64, RolloutPreflightRecord>,
    #[serde(default)]
    pub manifest: Option<RolloutManifest>,
    #[serde(default)]
    pub scenarios: BTreeMap<String, ScenarioAuthority>,
    #[serde(default)]
    pub hard_blockers: BTreeSet<ScenarioHardBlockerRecord>,
}

impl Default for RolloutState {
    fn default() -> Self {
        Self {
            protocol_mode: ProtocolMode::Legacy,
            config_revision: 0,
            latest_preflight_revision: 0,
            preflights: BTreeMap::new(),
            manifest: None,
            scenarios: BTreeMap::new(),
            hard_blockers: BTreeSet::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutPreflightRecord {
    pub revision: u64,
    pub manifest_revision: u64,
    pub state: RolloutPreflightState,
    #[serde(default)]
    pub manifest: Option<RolloutManifest>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutPreflightState {
    Open,
    Completed,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolMode {
    Legacy,
    Shadow,
    Authoritative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScenarioMode {
    Off,
    Shadow,
    Authoritative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutManifest {
    pub revision: u64,
    pub preflight_revision: u64,
    pub preflight_for_manifest_revision: u64,
    pub preflight_succeeded: bool,
    pub protocol_build: String,
    pub schema_build: String,
    pub schema_revision: u64,
    pub fixture_corpus_revision: String,
    pub classes: BTreeMap<String, RolloutClassEvidence>,
    pub safety_divergence_bps: u32,
    pub canonical_state_divergence_bps: u32,
    pub allowed_observational_divergence: BTreeMap<String, ObservationalDivergenceAllowance>,
    pub approver: String,
    pub approved_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationalDivergenceAllowance {
    pub maximum_rate_bps: u32,
    pub reviewed_by: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutClassEvidence {
    pub configured_mode: ScenarioMode,
    pub minimum_shadow_samples: u64,
    pub minimum_shadow_duration_secs: u64,
    pub observed_shadow_samples: u64,
    pub observed_shadow_duration_secs: u64,
    pub maximum_p99_latency_regression_bps: u32,
    pub observed_p99_latency_regression_bps: u32,
    pub hard_blocker_count: u64,
    pub unresolved_divergence_count: u64,
    pub required_evidence: BTreeSet<String>,
    pub verified_evidence: BTreeSet<String>,
    pub rollback_policy: RollbackPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackPolicy {
    pub trigger: RollbackTrigger,
    pub action: RollbackAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackTrigger {
    AnyHardBlocker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RollbackAction {
    StopAdmissionsAndRevert { target: ScenarioMode },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ScenarioHardBlockerRecord {
    pub scenario_class: String,
    pub blocker_code: String,
    pub config_revision: u64,
    pub manifest_revision: u64,
    pub preflight_revision: u64,
    pub trigger: RollbackTrigger,
    pub action: RollbackAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioAuthority {
    pub mode: ScenarioMode,
    pub rollback_target: ScenarioMode,
    #[serde(default)]
    pub manifest_revision: Option<u64>,
    #[serde(default)]
    pub preflight_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Admit {
        activation_id: String,
        work_item_id: String,
        expected_generation: u64,
        expected_dispatch_revision: u64,
        cause: AdmissionCause,
    },
    TriggerWait {
        wait_id: String,
        wait_generation: u64,
        trigger_id: String,
        trigger_generation: u64,
    },
    UpdateMetadata {
        work_item_id: String,
        expected_metadata_revision: u64,
    },
    ConfigureProtocol {
        expected_config_revision: u64,
        mode: ProtocolMode,
    },
    OpenRolloutPreflight {
        expected_config_revision: u64,
        manifest_revision: u64,
    },
    CompleteRolloutPreflight {
        expected_config_revision: u64,
        expected_preflight_revision: u64,
        manifest: RolloutManifest,
    },
    InstallRolloutManifest {
        expected_config_revision: u64,
        manifest: RolloutManifest,
    },
    ChangeScenarioAuthority {
        scenario_class: String,
        expected_config_revision: u64,
        expected_manifest_revision: u64,
        expected_preflight_revision: u64,
        mode: ScenarioMode,
    },
    ReportScenarioHardBlocker {
        scenario_class: String,
        blocker_code: String,
        expected_config_revision: u64,
        expected_manifest_revision: u64,
        expected_preflight_revision: u64,
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
    WaitResume {
        wait_id: String,
        wait_generation: u64,
        trigger_id: String,
        trigger_generation: u64,
    },
    SettlementRecovery {
        missing_activation_id: String,
    },
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
    MetadataUpdated,
    ProtocolConfigured,
    RolloutPreflightOpened,
    RolloutPreflightCompleted,
    ManifestInstalled,
    ScenarioAuthorityChanged,
    RollbackTripped,
    DuplicateIgnored,
    OperatorIntervention,
    SettlementMissing,
    SettlementHeld,
    Rejected,
}

pub fn reduce(snapshot: &Snapshot, event: &Event) -> Outcome {
    match event {
        Event::Admit {
            activation_id,
            work_item_id,
            expected_generation,
            expected_dispatch_revision,
            cause,
        } => admit(
            snapshot,
            activation_id,
            work_item_id,
            *expected_generation,
            *expected_dispatch_revision,
            cause,
        ),
        Event::TriggerWait {
            wait_id,
            wait_generation,
            trigger_id,
            trigger_generation,
        } => trigger_wait(
            snapshot,
            wait_id,
            *wait_generation,
            trigger_id,
            *trigger_generation,
        ),
        Event::UpdateMetadata {
            work_item_id,
            expected_metadata_revision,
        } => update_metadata(snapshot, work_item_id, *expected_metadata_revision),
        Event::ConfigureProtocol {
            expected_config_revision,
            mode,
        } => configure_protocol(snapshot, *expected_config_revision, *mode),
        Event::OpenRolloutPreflight {
            expected_config_revision,
            manifest_revision,
        } => open_rollout_preflight(snapshot, *expected_config_revision, *manifest_revision),
        Event::CompleteRolloutPreflight {
            expected_config_revision,
            expected_preflight_revision,
            manifest,
        } => complete_rollout_preflight(
            snapshot,
            *expected_config_revision,
            *expected_preflight_revision,
            manifest,
        ),
        Event::InstallRolloutManifest {
            expected_config_revision,
            manifest,
        } => install_rollout_manifest(snapshot, *expected_config_revision, manifest),
        Event::ChangeScenarioAuthority {
            scenario_class,
            expected_config_revision,
            expected_manifest_revision,
            expected_preflight_revision,
            mode,
        } => change_scenario_authority(
            snapshot,
            scenario_class,
            *expected_config_revision,
            *expected_manifest_revision,
            *expected_preflight_revision,
            *mode,
        ),
        Event::ReportScenarioHardBlocker {
            scenario_class,
            blocker_code,
            expected_config_revision,
            expected_manifest_revision,
            expected_preflight_revision,
        } => report_scenario_hard_blocker(
            snapshot,
            scenario_class,
            blocker_code,
            *expected_config_revision,
            *expected_manifest_revision,
            *expected_preflight_revision,
        ),
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
    expected_generation: u64,
    expected_dispatch_revision: u64,
    cause: &AdmissionCause,
) -> Outcome {
    if let Some(existing) = snapshot.activations.get(activation_id) {
        let diagnostic = match existing.state {
            ActivationState::Running => "activation_already_running",
            ActivationState::Settled => "activation_already_settled",
            ActivationState::SettlementMissing => "activation_settlement_missing",
        };
        return rejected(snapshot, diagnostic);
    }
    if !matches!(snapshot.slot, ActivationSlot::Idle) {
        return rejected(snapshot, "activation_slot_not_idle");
    }

    let Some(work) = snapshot.work.get(work_item_id) else {
        return rejected(snapshot, "unknown_work_item");
    };
    if work.scheduling_generation != expected_generation {
        return rejected(snapshot, "stale_scheduling_generation");
    }
    if snapshot.dispatch_revision != expected_dispatch_revision {
        return rejected(snapshot, "stale_dispatch_revision");
    }
    if !matches!(cause, AdmissionCause::SettlementRecovery { .. })
        && snapshot
            .work
            .values()
            .any(|work| matches!(work.status, WorkStatus::NeedsSettlement { .. }))
    {
        return rejected(snapshot, "settlement_recovery_pending");
    }

    let mut next = snapshot.clone();
    let mut transitions = Vec::new();
    let mut recovery_for = None;
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
            wait_generation,
            trigger_id,
            trigger_generation,
        } => {
            let Some(wait) = snapshot.waits.get(wait_id) else {
                return rejected(snapshot, "unknown_wait");
            };
            if wait.current_generation != *wait_generation {
                return rejected(snapshot, "stale_wait_generation");
            }
            let wait_generation_record = wait
                .generations
                .get(wait_generation)
                .expect("current wait generation exists");
            if wait_generation_record.owner_work_item_id != work_item_id {
                return rejected(snapshot, "wait_owner_mismatch");
            }
            if wait_generation_record.state != WaitState::Triggered {
                return rejected(snapshot, "wait_not_triggered");
            }
            if wait_generation_record.trigger
                != Some(WaitTrigger {
                    trigger_id: trigger_id.clone(),
                    trigger_generation: *trigger_generation,
                })
            {
                return rejected(snapshot, "wait_trigger_identity_mismatch");
            }
            if work.status
                != (WorkStatus::Waiting {
                    wait_id: wait_id.clone(),
                })
            {
                return rejected(snapshot, "work_item_not_waiting_for_wait");
            }
            if let AgentDispatchState::Awaiting {
                wait_id: reserved_wait,
            } = &snapshot.dispatch
            {
                if reserved_wait != wait_id {
                    return rejected(snapshot, "agent_lane_reserved_for_other_wait");
                }
            }

            let consumed = next
                .waits
                .get_mut(wait_id)
                .expect("wait exists")
                .generations
                .get_mut(wait_generation)
                .expect("current wait generation exists");
            consumed.state = WaitState::Consumed;
            consumed.consuming_activation_id = Some(activation_id.to_string());
            transitions.push(format!(
                "wait:{wait_id}:generation:{wait_generation}:triggered->consumed:{activation_id}"
            ));
            if matches!(
                snapshot.dispatch,
                AgentDispatchState::Awaiting { wait_id: ref reserved_wait }
                    if reserved_wait == wait_id
            ) {
                set_dispatch_state(&mut next, AgentDispatchState::Open);
            }
        }
        AdmissionCause::SettlementRecovery {
            missing_activation_id,
        } => {
            let Some(missing) = snapshot.activations.get(missing_activation_id) else {
                return rejected(snapshot, "unknown_missing_settlement");
            };
            if missing.work_item_id != work_item_id {
                return rejected(snapshot, "missing_settlement_owner_mismatch");
            }
            if missing.admitted_generation != expected_generation {
                return rejected(snapshot, "stale_recovery_generation");
            }
            if missing.state != ActivationState::SettlementMissing || missing.recovery_for.is_some()
            {
                return rejected(snapshot, "activation_is_not_canonical_missing_settlement");
            }
            if snapshot
                .activations
                .values()
                .any(|activation| activation.recovery_for.as_deref() == Some(missing_activation_id))
            {
                return rejected(snapshot, "settlement_recovery_already_attempted");
            }
            if work.status
                != (WorkStatus::NeedsSettlement {
                    activation_id: missing_activation_id.clone(),
                })
            {
                return rejected(snapshot, "work_item_not_awaiting_settlement_recovery");
            }
            recovery_for = Some(missing_activation_id.clone());
            transitions.push(format!(
                "settlement:{missing_activation_id}:awaiting_recovery->running:{activation_id}"
            ));
        }
    }

    let admission_fence = match cause {
        AdmissionCause::SettlementRecovery {
            missing_activation_id,
        } => format!("{work_item_id}:{expected_generation}:recovery:{missing_activation_id}"),
        _ => format!("{work_item_id}:{expected_generation}"),
    };
    if snapshot.admitted_generations.contains(&admission_fence) {
        return rejected(snapshot, "scheduling_generation_already_admitted");
    }
    next.admitted_generations.insert(admission_fence);
    next.activations.insert(
        activation_id.to_string(),
        ActivationRecord {
            work_item_id: work_item_id.to_string(),
            admitted_generation: expected_generation,
            state: ActivationState::Running,
            recovery_for: recovery_for.clone(),
        },
    );
    next.slot = ActivationSlot::Running {
        activation_id: activation_id.to_string(),
        work_item_id: work_item_id.to_string(),
        admitted_generation: expected_generation,
        recovery_for,
    };
    transitions.push(format!("slot:idle->running:{activation_id}"));
    Outcome {
        decision: Decision::Admitted,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn trigger_wait(
    snapshot: &Snapshot,
    wait_id: &str,
    wait_generation: u64,
    trigger_id: &str,
    trigger_generation: u64,
) -> Outcome {
    let Some(wait) = snapshot.waits.get(wait_id) else {
        return rejected(snapshot, "unknown_wait");
    };
    if wait.current_generation != wait_generation {
        return rejected(snapshot, "stale_wait_generation");
    }
    let generation = wait
        .generations
        .get(&wait_generation)
        .expect("current wait generation exists");
    if generation.state != WaitState::Active {
        if generation.trigger
            != Some(WaitTrigger {
                trigger_id: trigger_id.to_string(),
                trigger_generation,
            })
        {
            return rejected(snapshot, "conflicting_wait_trigger");
        }
        return Outcome {
            decision: Decision::DuplicateIgnored,
            transitions: Vec::new(),
            diagnostics: vec![format!(
                "wait_not_active:{wait_id}:{wait_generation}:{:?}",
                generation.state
            )],
            snapshot: snapshot.clone(),
        };
    }

    let mut next = snapshot.clone();
    let triggered = next
        .waits
        .get_mut(wait_id)
        .expect("wait exists")
        .generations
        .get_mut(&wait_generation)
        .expect("current wait generation exists");
    triggered.state = WaitState::Triggered;
    triggered.trigger = Some(WaitTrigger {
        trigger_id: trigger_id.to_string(),
        trigger_generation,
    });
    Outcome {
        decision: Decision::WaitTriggered,
        transitions: vec![format!(
            "wait:{wait_id}:generation:{wait_generation}:active->triggered:{trigger_id}:{trigger_generation}"
        )],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn settle(snapshot: &Snapshot, activation_id: &str, settlement: &Settlement) -> Outcome {
    let ActivationSlot::Running {
        activation_id: running_activation_id,
        work_item_id,
        admitted_generation,
        recovery_for,
    } = &snapshot.slot
    else {
        return rejected(snapshot, "no_running_activation");
    };
    if running_activation_id != activation_id {
        return rejected(snapshot, "activation_id_mismatch");
    }

    let Some(current_work) = snapshot.work.get(work_item_id) else {
        return rejected(snapshot, "running_work_item_missing");
    };
    if current_work.scheduling_generation != *admitted_generation {
        return rejected(snapshot, "stale_activation_generation");
    }
    let Some(running_activation) = snapshot.activations.get(activation_id) else {
        return rejected(snapshot, "running_activation_record_missing");
    };
    if running_activation.work_item_id != *work_item_id
        || running_activation.admitted_generation != *admitted_generation
        || running_activation.state != ActivationState::Running
        || running_activation.recovery_for != *recovery_for
    {
        return rejected(snapshot, "running_activation_record_mismatch");
    }
    if matches!(settlement, Settlement::Missing) {
        let mut next = snapshot.clone();
        next.slot = ActivationSlot::Idle;
        next.activations
            .get_mut(activation_id)
            .expect("running activation exists")
            .state = ActivationState::SettlementMissing;
        if let Some(missing_activation_id) = recovery_for {
            let hold_id = format!("settlement-recovery:{missing_activation_id}");
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Paused {
                hold_id: hold_id.clone(),
            };
            return Outcome {
                decision: Decision::SettlementHeld,
                transitions: vec![
                    format!("activation:{activation_id}:recovery_failed"),
                    format!("work:{work_item_id}:paused:{hold_id}"),
                ],
                diagnostics: vec![format!(
                    "settlement_recovery_failed:{missing_activation_id}:{activation_id}"
                )],
                snapshot: next,
            };
        }

        next.work
            .get_mut(work_item_id)
            .expect("running work item exists")
            .status = WorkStatus::NeedsSettlement {
            activation_id: activation_id.to_string(),
        };
        return Outcome {
            decision: Decision::SettlementMissing,
            transitions: vec![
                format!("activation:{activation_id}:running->settlement_missing"),
                format!("slot:running:{activation_id}->idle"),
                format!("work:{work_item_id}:needs_settlement:{activation_id}"),
            ],
            diagnostics: vec![format!("settlement_missing:{activation_id}")],
            snapshot: next,
        };
    }
    let settlement_owner_activation = recovery_for
        .as_deref()
        .unwrap_or(running_activation_id.as_str());
    let consumed_wait_id = match &current_work.status {
        WorkStatus::Waiting { wait_id }
            if snapshot.waits.get(wait_id).is_some_and(|wait| {
                wait.generations
                    .get(&wait.current_generation)
                    .is_some_and(|generation| {
                        generation.owner_work_item_id == *work_item_id
                            && generation.state == WaitState::Consumed
                            && generation.consuming_activation_id.as_deref()
                                == Some(settlement_owner_activation)
                    })
            }) =>
        {
            Some(wait_id.clone())
        }
        _ => None,
    };
    let mut next = snapshot.clone();
    let next_generation = current_work.scheduling_generation + 1;
    next.work
        .get_mut(work_item_id)
        .expect("running work item exists")
        .scheduling_generation = next_generation;

    let mut transitions = vec![
        format!("activation:{activation_id}:settled"),
        format!(
            "work:{work_item_id}:scheduling_generation:{}->{}",
            current_work.scheduling_generation, next_generation
        ),
    ];

    match settlement {
        Settlement::Continue | Settlement::Yield => {
            resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Runnable;
            set_dispatch_state(&mut next, AgentDispatchState::Open);
            transitions.push(format!("work:{work_item_id}:runnable"));
        }
        Settlement::Wait { wait_id, mode } => {
            if let Some(existing_wait) = next.waits.get(wait_id) {
                let current_generation = existing_wait
                    .generations
                    .get(&existing_wait.current_generation)
                    .expect("current wait generation exists");
                if matches!(
                    current_generation.state,
                    WaitState::Active | WaitState::Triggered
                ) {
                    return rejected(snapshot, "wait_id_still_active");
                }
                if current_generation.owner_work_item_id != *work_item_id {
                    return rejected(snapshot, "wait_id_owner_mismatch");
                }
                if existing_wait.current_generation >= next_generation {
                    return rejected(snapshot, "wait_generation_not_advanced");
                }
                if current_generation.state == WaitState::Consumed
                    && (consumed_wait_id.as_deref() != Some(wait_id)
                        || current_generation.consuming_activation_id.as_deref()
                            != Some(settlement_owner_activation))
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
            let previous_generation = next.waits.get(wait_id).map(|wait| wait.current_generation);
            if let Some(wait) = next.waits.get_mut(wait_id) {
                let previous = wait
                    .generations
                    .get_mut(&wait.current_generation)
                    .expect("current wait generation exists");
                if previous.state == WaitState::Consumed {
                    previous.state = WaitState::Resolved;
                    transitions.push(format!(
                        "wait:{wait_id}:generation:{}:consumed->resolved",
                        wait.current_generation
                    ));
                }
                wait.current_generation = next_generation;
                wait.generations.insert(
                    next_generation,
                    WaitGenerationRecord {
                        owner_work_item_id: work_item_id.clone(),
                        state: WaitState::Active,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                );
            } else {
                next.waits.insert(
                    wait_id.clone(),
                    WaitRecord {
                        current_generation: next_generation,
                        generations: BTreeMap::from([(
                            next_generation,
                            WaitGenerationRecord {
                                owner_work_item_id: work_item_id.clone(),
                                state: WaitState::Active,
                                trigger: None,
                                consuming_activation_id: None,
                            },
                        )]),
                    },
                );
            }
            let dispatch = match mode {
                WaitMode::AwaitThis => AgentDispatchState::Awaiting {
                    wait_id: wait_id.clone(),
                },
                WaitMode::AcceptScheduling => AgentDispatchState::Open,
            };
            set_dispatch_state(&mut next, dispatch);
            transitions.push(match previous_generation {
                Some(generation) => {
                    format!("wait:{wait_id}:generation:{generation}->{next_generation}:active")
                }
                None => format!("wait:{wait_id}:generation:{next_generation}:created:active"),
            });
        }
        Settlement::Complete { continuation } => {
            resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Terminal;
            set_dispatch_state(&mut next, AgentDispatchState::Open);
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
                caller.scheduling_generation += 1;
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
    next.activations
        .get_mut(running_activation_id)
        .expect("running activation exists")
        .state = ActivationState::Settled;
    if let Some(missing_activation_id) = recovery_for {
        next.activations
            .get_mut(missing_activation_id)
            .expect("missing activation exists")
            .state = ActivationState::Settled;
        transitions.push(format!(
            "settlement:{missing_activation_id}:recovered:{activation_id}"
        ));
    }
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
    let generation = wait
        .generations
        .get_mut(&wait.current_generation)
        .expect("current wait generation exists");
    generation.state = WaitState::Resolved;
    transitions.push(format!(
        "wait:{wait_id}:generation:{}:consumed->resolved",
        wait.current_generation
    ));
}

fn set_dispatch_state(snapshot: &mut Snapshot, dispatch: AgentDispatchState) {
    if snapshot.dispatch != dispatch {
        snapshot.dispatch = dispatch;
        snapshot.dispatch_revision += 1;
    }
}

fn update_metadata(
    snapshot: &Snapshot,
    work_item_id: &str,
    expected_metadata_revision: u64,
) -> Outcome {
    let Some(work) = snapshot.work.get(work_item_id) else {
        return rejected(snapshot, "unknown_work_item");
    };
    if work.metadata_revision != expected_metadata_revision {
        return rejected(snapshot, "stale_metadata_revision");
    }
    let mut next = snapshot.clone();
    next.work
        .get_mut(work_item_id)
        .expect("work item exists")
        .metadata_revision += 1;
    Outcome {
        decision: Decision::MetadataUpdated,
        transitions: vec![format!(
            "work:{work_item_id}:metadata_revision:{expected_metadata_revision}->{}",
            expected_metadata_revision + 1
        )],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn configure_protocol(
    snapshot: &Snapshot,
    expected_config_revision: u64,
    mode: ProtocolMode,
) -> Outcome {
    if snapshot.rollout.config_revision != expected_config_revision {
        return rejected(snapshot, "stale_rollout_config_revision");
    }
    if mode != ProtocolMode::Legacy && snapshot.rollout.manifest.is_none() {
        return rejected(snapshot, "non_legacy_protocol_requires_manifest");
    }
    if snapshot
        .rollout
        .scenarios
        .values()
        .any(|scenario| !scenario_allowed(mode, scenario.mode))
    {
        return rejected(snapshot, "scenario_exceeds_protocol_ceiling");
    }
    let mut next = snapshot.clone();
    next.rollout.protocol_mode = mode;
    next.rollout.config_revision += 1;
    Outcome {
        decision: Decision::ProtocolConfigured,
        transitions: vec![format!(
            "rollout:protocol:{:?}->{mode:?}",
            snapshot.rollout.protocol_mode
        )],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn open_rollout_preflight(
    snapshot: &Snapshot,
    expected_config_revision: u64,
    manifest_revision: u64,
) -> Outcome {
    if snapshot.rollout.config_revision != expected_config_revision {
        return rejected(snapshot, "stale_rollout_config_revision");
    }
    if snapshot
        .rollout
        .manifest
        .as_ref()
        .is_some_and(|current| manifest_revision <= current.revision)
    {
        return rejected(snapshot, "manifest_revision_not_advanced");
    }
    if snapshot
        .rollout
        .preflights
        .values()
        .any(|preflight| preflight.state == RolloutPreflightState::Open)
    {
        return rejected(snapshot, "rollout_preflight_window_already_open");
    }

    let mut next = snapshot.clone();
    let revision = next.rollout.latest_preflight_revision + 1;
    next.rollout.latest_preflight_revision = revision;
    next.rollout.preflights.insert(
        revision,
        RolloutPreflightRecord {
            revision,
            manifest_revision,
            state: RolloutPreflightState::Open,
            manifest: None,
        },
    );
    Outcome {
        decision: Decision::RolloutPreflightOpened,
        transitions: vec![format!(
            "rollout:preflight:{revision}:opened:manifest:{manifest_revision}"
        )],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn complete_rollout_preflight(
    snapshot: &Snapshot,
    expected_config_revision: u64,
    expected_preflight_revision: u64,
    manifest: &RolloutManifest,
) -> Outcome {
    if snapshot.rollout.config_revision != expected_config_revision {
        return rejected(snapshot, "stale_rollout_config_revision");
    }
    let Some(preflight) = snapshot
        .rollout
        .preflights
        .get(&expected_preflight_revision)
    else {
        return rejected(snapshot, "rollout_preflight_record_missing");
    };
    if preflight.state != RolloutPreflightState::Open {
        return rejected(snapshot, "rollout_preflight_window_not_open");
    }
    if manifest.preflight_revision != expected_preflight_revision
        || manifest.preflight_for_manifest_revision != preflight.manifest_revision
        || manifest.revision != preflight.manifest_revision
    {
        return rejected(snapshot, "rollout_preflight_binding_mismatch");
    }
    if !manifest.preflight_succeeded {
        return rejected(snapshot, "rollout_preflight_failed");
    }
    if !rollout_manifest_is_complete(manifest) {
        return rejected(snapshot, "rollout_manifest_incomplete");
    }

    let mut next = snapshot.clone();
    let preflight = next
        .rollout
        .preflights
        .get_mut(&expected_preflight_revision)
        .expect("preflight exists");
    preflight.state = RolloutPreflightState::Completed;
    preflight.manifest = Some(manifest.clone());
    Outcome {
        decision: Decision::RolloutPreflightCompleted,
        transitions: vec![format!(
            "rollout:preflight:{expected_preflight_revision}:completed:manifest:{}",
            manifest.revision
        )],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn install_rollout_manifest(
    snapshot: &Snapshot,
    expected_config_revision: u64,
    manifest: &RolloutManifest,
) -> Outcome {
    if snapshot.rollout.config_revision != expected_config_revision {
        return rejected(snapshot, "stale_rollout_config_revision");
    }
    if snapshot
        .rollout
        .manifest
        .as_ref()
        .is_some_and(|current| manifest.revision <= current.revision)
    {
        return rejected(snapshot, "manifest_revision_not_advanced");
    }
    if !manifest.preflight_succeeded {
        return rejected(snapshot, "rollout_preflight_failed");
    }
    if !rollout_manifest_is_complete(manifest) {
        return rejected(snapshot, "rollout_manifest_incomplete");
    }
    let Some(preflight) = snapshot
        .rollout
        .preflights
        .get(&manifest.preflight_revision)
    else {
        return rejected(snapshot, "rollout_preflight_record_missing");
    };
    if preflight.state != RolloutPreflightState::Completed {
        return rejected(snapshot, "rollout_preflight_record_not_installable");
    }
    if preflight.manifest_revision != manifest.revision
        || preflight.manifest.as_ref() != Some(manifest)
    {
        return rejected(snapshot, "rollout_preflight_record_mismatch");
    }
    let mut next = snapshot.clone();
    let mut transitions = vec![format!("rollout:manifest:installed:{}", manifest.revision)];
    for (scenario_class, scenario) in &mut next.rollout.scenarios {
        if scenario.mode == ScenarioMode::Authoritative {
            transitions.push(format!(
                "rollout:scenario:{scenario_class}:authoritative->{:?}:manifest_replaced",
                scenario.rollback_target
            ));
            scenario.mode = scenario.rollback_target;
        }
        scenario.manifest_revision = None;
        scenario.preflight_revision = None;
    }
    next.rollout
        .preflights
        .get_mut(&manifest.preflight_revision)
        .expect("preflight exists")
        .state = RolloutPreflightState::Consumed;
    next.rollout.manifest = Some(manifest.clone());
    next.rollout.config_revision += 1;
    Outcome {
        decision: Decision::ManifestInstalled,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn change_scenario_authority(
    snapshot: &Snapshot,
    scenario_class: &str,
    expected_config_revision: u64,
    expected_manifest_revision: u64,
    expected_preflight_revision: u64,
    mode: ScenarioMode,
) -> Outcome {
    if snapshot.rollout.config_revision != expected_config_revision {
        return rejected(snapshot, "stale_rollout_config_revision");
    }
    if !scenario_allowed(snapshot.rollout.protocol_mode, mode) {
        return rejected(snapshot, "scenario_exceeds_protocol_ceiling");
    }
    let Some(manifest) = snapshot.rollout.manifest.as_ref() else {
        return rejected(snapshot, "rollout_manifest_missing");
    };
    if manifest.revision != expected_manifest_revision {
        return rejected(snapshot, "stale_rollout_manifest_revision");
    }
    if manifest.preflight_revision != expected_preflight_revision {
        return rejected(snapshot, "stale_rollout_preflight_revision");
    }
    let class = manifest.classes.get(scenario_class);
    if mode == ScenarioMode::Shadow
        && !class.is_some_and(|class| {
            matches!(
                class.configured_mode,
                ScenarioMode::Shadow | ScenarioMode::Authoritative
            )
        })
    {
        return rejected(snapshot, "scenario_not_enabled_by_manifest");
    }
    if mode == ScenarioMode::Authoritative {
        if !snapshot
            .rollout
            .scenarios
            .get(scenario_class)
            .is_some_and(|scenario| scenario.mode == ScenarioMode::Shadow)
        {
            return rejected(snapshot, "scenario_not_shadow");
        }
        if !manifest.preflight_succeeded {
            return rejected(snapshot, "rollout_preflight_failed");
        }
        let Some(class) = class else {
            return rejected(snapshot, "scenario_not_approved_for_authority");
        };
        if class.configured_mode != ScenarioMode::Authoritative {
            return rejected(snapshot, "scenario_not_approved_for_authority");
        }
    }
    let rollback_target = manifest
        .classes
        .get(scenario_class)
        .map(|class| rollback_target(&class.rollback_policy))
        .unwrap_or(ScenarioMode::Off);
    if rollback_target == ScenarioMode::Authoritative {
        return rejected(snapshot, "invalid_authoritative_rollback_target");
    }
    let mut next = snapshot.clone();
    next.rollout.scenarios.insert(
        scenario_class.to_string(),
        ScenarioAuthority {
            mode,
            rollback_target,
            manifest_revision: (mode == ScenarioMode::Authoritative).then_some(manifest.revision),
            preflight_revision: (mode == ScenarioMode::Authoritative)
                .then_some(manifest.preflight_revision),
        },
    );
    next.rollout.config_revision += 1;
    Outcome {
        decision: Decision::ScenarioAuthorityChanged,
        transitions: vec![format!("rollout:scenario:{scenario_class}:{mode:?}")],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn report_scenario_hard_blocker(
    snapshot: &Snapshot,
    scenario_class: &str,
    blocker_code: &str,
    expected_config_revision: u64,
    expected_manifest_revision: u64,
    expected_preflight_revision: u64,
) -> Outcome {
    if snapshot.rollout.config_revision != expected_config_revision {
        return rejected(snapshot, "stale_rollout_config_revision");
    }
    let Some(scenario) = snapshot.rollout.scenarios.get(scenario_class) else {
        return rejected(snapshot, "unknown_scenario_class");
    };
    if scenario.mode != ScenarioMode::Authoritative {
        return rejected(snapshot, "scenario_not_authoritative");
    }
    if scenario.manifest_revision != Some(expected_manifest_revision) {
        return rejected(snapshot, "stale_rollout_manifest_revision");
    }
    if scenario.preflight_revision != Some(expected_preflight_revision) {
        return rejected(snapshot, "stale_rollout_preflight_revision");
    }
    if blocker_code.is_empty() {
        return rejected(snapshot, "hard_blocker_code_missing");
    }
    let Some(class) = snapshot
        .rollout
        .manifest
        .as_ref()
        .and_then(|manifest| manifest.classes.get(scenario_class))
    else {
        return rejected(snapshot, "scenario_not_approved_for_authority");
    };
    if class.rollback_policy.trigger != RollbackTrigger::AnyHardBlocker {
        return rejected(snapshot, "hard_blocker_trigger_not_configured");
    }
    let rollback_trigger = class.rollback_policy.trigger;
    let rollback_action = class.rollback_policy.action;
    let rollback_target = rollback_target(&class.rollback_policy);
    let mut next = snapshot.clone();
    next.rollout
        .hard_blockers
        .insert(ScenarioHardBlockerRecord {
            scenario_class: scenario_class.to_string(),
            blocker_code: blocker_code.to_string(),
            config_revision: expected_config_revision,
            manifest_revision: expected_manifest_revision,
            preflight_revision: expected_preflight_revision,
            trigger: rollback_trigger,
            action: rollback_action,
        });
    let rolled_back = next
        .rollout
        .scenarios
        .get_mut(scenario_class)
        .expect("scenario exists");
    rolled_back.mode = rollback_target;
    rolled_back.manifest_revision = None;
    rolled_back.preflight_revision = None;
    next.rollout.config_revision += 1;
    Outcome {
        decision: Decision::RollbackTripped,
        transitions: vec![
            format!("rollout:hard_blocker:{scenario_class}:{blocker_code}"),
            format!(
                "rollout:scenario:{scenario_class}:authoritative->{rollback_target:?}:stop_admissions_and_revert"
            ),
        ],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn rollback_target(policy: &RollbackPolicy) -> ScenarioMode {
    match policy.action {
        RollbackAction::StopAdmissionsAndRevert { target } => target,
    }
}

fn scenario_allowed(protocol_mode: ProtocolMode, scenario_mode: ScenarioMode) -> bool {
    matches!(
        (protocol_mode, scenario_mode),
        (_, ScenarioMode::Off)
            | (ProtocolMode::Shadow, ScenarioMode::Shadow)
            | (
                ProtocolMode::Authoritative,
                ScenarioMode::Shadow | ScenarioMode::Authoritative
            )
    )
}

struct RolloutClassGate {
    minimum_shadow_samples: u64,
    minimum_shadow_duration_secs: u64,
    required_evidence: &'static [&'static str],
}

const UNIVERSAL_ROLLOUT_EVIDENCE: &[&str] = &["restart", "fault_injection", "rollback_drill"];
const MAXIMUM_P99_LATENCY_REGRESSION_BPS: u32 = 1_000;
const MAXIMUM_OBSERVATIONAL_DIVERGENCE_BPS: u32 = 100;

fn rollout_class_gate(scenario_class: &str) -> Option<RolloutClassGate> {
    let gate = match scenario_class {
        "reducer_only_candidates" => RolloutClassGate {
            minimum_shadow_samples: 10_000,
            minimum_shadow_duration_secs: 72 * 60 * 60,
            required_evidence: &["deterministic_replay", "duplicate_command_idempotency"],
        },
        "exact_task_rejoin" => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_task_result",
                "out_of_order_task_result",
                "restart_before_rejoin_settlement",
            ],
        },
        "exact_wait_resume" => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_trigger",
                "stale_generation",
                "restart_after_consume",
                "rearm",
            ],
        },
        "explicitly_bound_operator_input" => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_ingress",
                "stale_binding_revision",
                "wrong_agent_target",
            ],
        },
        "work_item_autonomous_continuation" => RolloutClassGate {
            minimum_shadow_samples: 2_000,
            minimum_shadow_duration_secs: 14 * 24 * 60 * 60,
            required_evidence: &[
                "concurrent_claim",
                "reservation_conflict",
                "yield_return",
                "work_item_rollback",
            ],
        },
        "ordinary_semantic_operator_binding" => RolloutClassGate {
            minimum_shadow_samples: 5_000,
            minimum_shadow_duration_secs: 14 * 24 * 60 * 60,
            required_evidence: &[
                "ambiguous_input",
                "low_confidence_input",
                "conflicting_proposals",
                "zero_wrong_automatic_bindings",
            ],
        },
        _ => return None,
    };
    Some(gate)
}

fn rollout_class_evidence_is_complete(scenario_class: &str, class: &RolloutClassEvidence) -> bool {
    let Some(gate) = rollout_class_gate(scenario_class) else {
        return false;
    };
    let mandatory_evidence: BTreeSet<&str> = UNIVERSAL_ROLLOUT_EVIDENCE
        .iter()
        .chain(gate.required_evidence.iter())
        .copied()
        .collect();

    class.minimum_shadow_samples >= gate.minimum_shadow_samples
        && class.minimum_shadow_duration_secs >= gate.minimum_shadow_duration_secs
        && class.observed_shadow_samples >= class.minimum_shadow_samples
        && class.observed_shadow_duration_secs >= class.minimum_shadow_duration_secs
        && class.maximum_p99_latency_regression_bps <= MAXIMUM_P99_LATENCY_REGRESSION_BPS
        && class.observed_p99_latency_regression_bps <= class.maximum_p99_latency_regression_bps
        && class.hard_blocker_count == 0
        && class.unresolved_divergence_count == 0
        && mandatory_evidence
            .iter()
            .all(|evidence| class.required_evidence.contains(*evidence))
        && mandatory_evidence
            .iter()
            .all(|evidence| class.verified_evidence.contains(*evidence))
        && class
            .required_evidence
            .iter()
            .all(|evidence| class.verified_evidence.contains(evidence))
        && class.rollback_policy.trigger == RollbackTrigger::AnyHardBlocker
        && rollback_target(&class.rollback_policy) != ScenarioMode::Authoritative
}

fn rollout_manifest_is_complete(manifest: &RolloutManifest) -> bool {
    manifest.preflight_for_manifest_revision == manifest.revision
        && manifest.preflight_succeeded
        && !manifest.protocol_build.is_empty()
        && !manifest.schema_build.is_empty()
        && manifest.schema_revision > 0
        && !manifest.fixture_corpus_revision.is_empty()
        && !manifest.classes.is_empty()
        && manifest.classes.iter().all(|(scenario_class, class)| {
            rollout_class_evidence_is_complete(scenario_class, class)
        })
        && manifest.safety_divergence_bps == 0
        && manifest.canonical_state_divergence_bps == 0
        && manifest
            .allowed_observational_divergence
            .iter()
            .all(|(code, allowance)| {
                !code.is_empty()
                    && allowance.maximum_rate_bps <= MAXIMUM_OBSERVATIONAL_DIVERGENCE_BPS
                    && !allowance.reviewed_by.is_empty()
            })
        && !manifest.approver.is_empty()
        && !manifest.approved_at.is_empty()
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
    if snapshot.rollout.protocol_mode != ProtocolMode::Legacy && snapshot.rollout.manifest.is_none()
    {
        return Err("non-legacy protocol has no rollout manifest".into());
    }
    if let Some(manifest) = &snapshot.rollout.manifest {
        if !rollout_manifest_is_complete(manifest) {
            return Err("rollout manifest is incomplete".into());
        }
        let preflight = snapshot
            .rollout
            .preflights
            .get(&manifest.preflight_revision)
            .ok_or_else(|| "rollout manifest has no canonical preflight".to_string())?;
        if preflight.state != RolloutPreflightState::Consumed
            || preflight.manifest_revision != manifest.revision
            || preflight.manifest.as_ref() != Some(manifest)
        {
            return Err("rollout manifest disagrees with canonical preflight".into());
        }
    }
    if snapshot
        .rollout
        .preflights
        .iter()
        .any(|(revision, preflight)| {
            *revision != preflight.revision
                || *revision > snapshot.rollout.latest_preflight_revision
                || match preflight.state {
                    RolloutPreflightState::Open => preflight.manifest.is_some(),
                    RolloutPreflightState::Completed | RolloutPreflightState::Consumed => {
                        preflight.manifest.as_ref().is_none_or(|manifest| {
                            manifest.revision != preflight.manifest_revision
                                || manifest.preflight_revision != preflight.revision
                                || manifest.preflight_for_manifest_revision
                                    != preflight.manifest_revision
                        })
                    }
                }
        })
    {
        return Err("rollout preflight record is invalid".into());
    }
    for blocker in &snapshot.rollout.hard_blockers {
        if blocker.scenario_class.is_empty()
            || blocker.blocker_code.is_empty()
            || blocker.trigger != RollbackTrigger::AnyHardBlocker
            || rollback_target(&RollbackPolicy {
                trigger: blocker.trigger,
                action: blocker.action,
            }) == ScenarioMode::Authoritative
        {
            return Err("rollout hard blocker record is invalid".into());
        }
    }
    for (scenario_class, scenario) in &snapshot.rollout.scenarios {
        if !scenario_allowed(snapshot.rollout.protocol_mode, scenario.mode) {
            return Err(format!(
                "scenario {scenario_class} exceeds the protocol ceiling"
            ));
        }
        match scenario.mode {
            ScenarioMode::Authoritative => {
                let manifest = snapshot
                    .rollout
                    .manifest
                    .as_ref()
                    .ok_or_else(|| "authoritative scenario has no manifest".to_string())?;
                let class = manifest
                    .classes
                    .get(scenario_class)
                    .ok_or_else(|| "authoritative scenario has no class evidence".to_string())?;
                if !manifest.preflight_succeeded
                    || class.configured_mode != ScenarioMode::Authoritative
                    || scenario.manifest_revision != Some(manifest.revision)
                    || scenario.preflight_revision != Some(manifest.preflight_revision)
                    || scenario.rollback_target != rollback_target(&class.rollback_policy)
                {
                    return Err(format!(
                        "authoritative scenario {scenario_class} has stale rollout evidence"
                    ));
                }
            }
            ScenarioMode::Off | ScenarioMode::Shadow => {
                if scenario.manifest_revision.is_some() || scenario.preflight_revision.is_some() {
                    return Err(format!(
                        "non-authoritative scenario {scenario_class} retains authority fences"
                    ));
                }
            }
        }
    }

    match &snapshot.slot {
        ActivationSlot::Idle => {}
        ActivationSlot::Running {
            activation_id,
            work_item_id,
            admitted_generation,
            recovery_for,
        } => {
            let activation = snapshot
                .activations
                .get(activation_id)
                .ok_or_else(|| "running slot has no canonical activation".to_string())?;
            if activation.work_item_id != *work_item_id
                || activation.admitted_generation != *admitted_generation
                || activation.state != ActivationState::Running
                || activation.recovery_for != *recovery_for
            {
                return Err("running slot disagrees with canonical activation".into());
            }
            let work = snapshot
                .work
                .get(work_item_id)
                .ok_or_else(|| "running activation references unknown work item".to_string())?;
            if work.scheduling_generation != *admitted_generation {
                return Err("running activation generation fence does not match work item".into());
            }
            match recovery_for {
                Some(missing_activation_id) => {
                    let missing =
                        snapshot
                            .activations
                            .get(missing_activation_id)
                            .ok_or_else(|| {
                                "recovery activation references unknown missing settlement"
                                    .to_string()
                            })?;
                    if missing.work_item_id != *work_item_id
                        || missing.admitted_generation != *admitted_generation
                        || missing.state != ActivationState::SettlementMissing
                        || missing.recovery_for.is_some()
                        || work.status
                            != (WorkStatus::NeedsSettlement {
                                activation_id: missing_activation_id.clone(),
                            })
                    {
                        return Err(
                            "recovery activation is not paired with canonical settlement facts"
                                .into(),
                        );
                    }
                }
                None if matches!(work.status, WorkStatus::NeedsSettlement { .. }) => {
                    return Err(
                        "ordinary running activation has settlement-missing work state".into(),
                    );
                }
                None => {}
            }
        }
    }

    for (work_item_id, work) in &snapshot.work {
        if let WorkStatus::NeedsSettlement { activation_id } = &work.status {
            let missing = snapshot
                .activations
                .get(activation_id)
                .ok_or_else(|| "needs-settlement work item has no canonical fact".to_string())?;
            if missing.work_item_id != *work_item_id
                || missing.admitted_generation != work.scheduling_generation
                || missing.state != ActivationState::SettlementMissing
                || missing.recovery_for.is_some()
            {
                return Err("needs-settlement work item has inconsistent canonical fact".into());
            }
        }
    }

    for (activation_id, activation) in &snapshot.activations {
        let work = snapshot
            .work
            .get(&activation.work_item_id)
            .ok_or_else(|| "activation references unknown work item".to_string())?;
        match activation.state {
            ActivationState::Running => {
                if snapshot.slot
                    != (ActivationSlot::Running {
                        activation_id: activation_id.clone(),
                        work_item_id: activation.work_item_id.clone(),
                        admitted_generation: activation.admitted_generation,
                        recovery_for: activation.recovery_for.clone(),
                    })
                {
                    return Err("running activation does not own the slot".into());
                }
            }
            ActivationState::Settled => {
                if matches!(
                    snapshot.slot,
                    ActivationSlot::Running {
                        activation_id: ref slot_activation_id,
                        ..
                    } if slot_activation_id == activation_id
                ) {
                    return Err("settled activation still owns the slot".into());
                }
            }
            ActivationState::SettlementMissing => {
                if let Some(missing_activation_id) = &activation.recovery_for {
                    let hold_id = format!("settlement-recovery:{missing_activation_id}");
                    if work.status != (WorkStatus::Paused { hold_id }) {
                        return Err(
                            "failed settlement recovery is not paired with a typed hold".into()
                        );
                    }
                } else {
                    let recovery = snapshot.activations.values().find(|candidate| {
                        candidate.recovery_for.as_deref() == Some(activation_id.as_str())
                    });
                    match recovery.map(|candidate| &candidate.state) {
                        Some(ActivationState::SettlementMissing)
                            if work.status
                                == (WorkStatus::Paused {
                                    hold_id: format!("settlement-recovery:{activation_id}"),
                                }) => {}
                        Some(ActivationState::Running) | None
                            if work.status
                                == (WorkStatus::NeedsSettlement {
                                    activation_id: activation_id.clone(),
                                }) => {}
                        _ => {
                            return Err(
                                "settlement-missing activation has inconsistent recovery state"
                                    .into(),
                            );
                        }
                    }
                }
            }
        }
    }

    if let AgentDispatchState::Awaiting { wait_id } = &snapshot.dispatch {
        let wait = snapshot
            .waits
            .get(wait_id)
            .ok_or_else(|| "lane reservation references unknown wait".to_string())?;
        let generation = wait
            .generations
            .get(&wait.current_generation)
            .ok_or_else(|| "lane reservation references missing wait generation".to_string())?;
        if !matches!(generation.state, WaitState::Active | WaitState::Triggered) {
            return Err("lane reservation references inactive wait".into());
        }
        let work = snapshot
            .work
            .get(&generation.owner_work_item_id)
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
        let current = wait
            .generations
            .get(&wait.current_generation)
            .ok_or_else(|| format!("wait {wait_id} is missing its current generation"))?;
        for (generation, record) in &wait.generations {
            if *generation != wait.current_generation && record.state != WaitState::Resolved {
                return Err(format!(
                    "wait {wait_id} has non-resolved historical generation {generation}"
                ));
            }
        }
        if matches!(
            current.state,
            WaitState::Active | WaitState::Triggered | WaitState::Consumed
        ) {
            let owner = snapshot
                .work
                .get(&current.owner_work_item_id)
                .ok_or_else(|| format!("wait {wait_id} references unknown owner"))?;
            if wait.current_generation != owner.scheduling_generation {
                return Err(format!(
                    "current wait {wait_id} generation does not match owner scheduling generation"
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
        match current.state {
            WaitState::Active
                if current.trigger.is_some() || current.consuming_activation_id.is_some() =>
            {
                return Err(format!(
                    "active wait {wait_id} carries trigger or consumer facts"
                ));
            }
            WaitState::Triggered
                if current.trigger.is_none() || current.consuming_activation_id.is_some() =>
            {
                return Err(format!(
                    "triggered wait {wait_id} has invalid trigger facts"
                ));
            }
            WaitState::Consumed => {
                let consuming_activation_id = current
                    .consuming_activation_id
                    .as_ref()
                    .ok_or_else(|| format!("consumed wait {wait_id} has no consumer"))?;
                if current.trigger.is_none()
                    || snapshot.slot
                        != (ActivationSlot::Running {
                            activation_id: consuming_activation_id.clone(),
                            work_item_id: current.owner_work_item_id.clone(),
                            admitted_generation: wait.current_generation,
                            recovery_for: None,
                        })
                {
                    return Err(format!(
                        "consumed wait {wait_id} has no matching running activation"
                    ));
                }
            }
            WaitState::Resolved | WaitState::Active | WaitState::Triggered => {}
        }
    }

    Ok(())
}
