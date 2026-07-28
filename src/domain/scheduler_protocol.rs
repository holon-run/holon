//! Pure deterministic scheduler protocol kernel.
//!
//! This module is the production home of the executable Scheduler / WorkItem
//! baseline. It is intentionally storage-independent and has no call site in
//! the production scheduler while the legacy scheduler remains authoritative.

use std::collections::{BTreeMap, BTreeSet};

use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "SnapshotWire")]
pub struct Snapshot {
    pub slot: ActivationSlot,
    pub dispatch: AgentDispatchState,
    pub dispatch_revision: u64,
    #[serde(default)]
    pub focus: Option<String>,
    pub work: BTreeMap<String, WorkDemand>,
    pub waits: BTreeMap<String, WaitRecord>,
    pub activations: BTreeMap<String, ActivationRecord>,
    pub activation_authorities: BTreeMap<String, ActivationAdmissionAuthority>,
    pub activation_admissions: BTreeMap<String, AdmitActivationCommand>,
    pub settlements: BTreeMap<String, ActivationSettlement>,
    pub missing_settlements: BTreeMap<String, MissingSettlementRecord>,
    pub rollout: RolloutState,
    pub admitted_generations: BTreeSet<String>,
    pub continuation_admissions: BTreeMap<String, ContinuationAdmissionRecord>,
    #[serde(default)]
    pub activation_inputs: BTreeMap<String, ActivationInputAttachment>,
}

#[derive(Deserialize)]
struct SnapshotWire {
    slot: ActivationSlot,
    dispatch: AgentDispatchStateWire,
    dispatch_revision: Option<u64>,
    #[serde(default)]
    focus: Option<String>,
    work: Option<BTreeMap<String, WorkDemand>>,
    waits: Option<BTreeMap<String, WaitRecord>>,
    activations: Option<BTreeMap<String, ActivationRecord>>,
    activation_authorities: Option<BTreeMap<String, ActivationAdmissionAuthority>>,
    activation_admissions: Option<BTreeMap<String, AdmitActivationCommand>>,
    settlements: Option<BTreeMap<String, ActivationSettlement>>,
    missing_settlements: Option<BTreeMap<String, MissingSettlementRecord>>,
    rollout: Option<RolloutState>,
    admitted_generations: Option<BTreeSet<String>>,
    continuation_admissions: Option<BTreeMap<String, ContinuationAdmissionRecord>>,
    #[serde(default)]
    activation_inputs: Option<BTreeMap<String, ActivationInputAttachment>>,
}

impl TryFrom<SnapshotWire> for Snapshot {
    type Error = String;

    fn try_from(wire: SnapshotWire) -> Result<Self, Self::Error> {
        let dispatch_revision = wire
            .dispatch_revision
            .ok_or_else(|| "snapshot is missing canonical dispatch revision".to_string())?;
        let work = wire
            .work
            .ok_or_else(|| "snapshot is missing canonical work demands".to_string())?;
        let waits = wire
            .waits
            .ok_or_else(|| "snapshot is missing canonical wait records".to_string())?;
        let activations = wire
            .activations
            .ok_or_else(|| "snapshot is missing canonical activation records".to_string())?;
        let rollout = wire
            .rollout
            .ok_or_else(|| "snapshot is missing canonical rollout state".to_string())?;
        let dispatch = wire.dispatch.into_snapshot_dispatch(&waits)?;
        let activation_authorities = wire
            .activation_authorities
            .ok_or_else(|| "snapshot is missing canonical activation authorities".to_string())?;
        let activation_admissions = wire
            .activation_admissions
            .ok_or_else(|| "snapshot is missing canonical activation admissions".to_string())?;
        let settlements = wire
            .settlements
            .ok_or_else(|| "snapshot is missing canonical activation settlements".to_string())?;
        let missing_settlements = wire.missing_settlements.ok_or_else(|| {
            "snapshot is missing canonical missing-settlement records".to_string()
        })?;
        let admitted_generations = wire
            .admitted_generations
            .ok_or_else(|| "snapshot is missing canonical admission fences".to_string())?;
        let continuation_admissions = wire
            .continuation_admissions
            .ok_or_else(|| "snapshot is missing canonical continuation admissions".to_string())?;
        let snapshot = Self {
            slot: wire.slot,
            dispatch,
            dispatch_revision,
            focus: wire.focus,
            work,
            waits,
            activations,
            activation_authorities,
            activation_admissions,
            settlements,
            missing_settlements,
            rollout,
            admitted_generations,
            continuation_admissions,
            activation_inputs: wire.activation_inputs.unwrap_or_default(),
        };
        assert_invariants(&snapshot)?;
        Ok(snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivationSlot {
    Idle,
    Running {
        activation_id: String,
        owner: SchedulerOwner,
        admitted_generation: u64,
        #[serde(default)]
        recovery_for: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchedulerOwner {
    WorkItem { work_item_id: String },
    AgentLifecycle { agent_id: String },
}

impl SchedulerOwner {
    pub fn work_item_id(&self) -> Option<&str> {
        match self {
            Self::WorkItem { work_item_id } => Some(work_item_id),
            Self::AgentLifecycle { .. } => None,
        }
    }

    pub fn lifecycle_agent_id(&self) -> Option<&str> {
        match self {
            Self::AgentLifecycle { agent_id } => Some(agent_id),
            Self::WorkItem { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDispatchState {
    Open,
    Awaiting { wait: WaitIdentity },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum AgentDispatchStateWire {
    Open,
    Awaiting {
        #[serde(default)]
        wait: Option<WaitIdentity>,
        #[serde(default)]
        wait_id: Option<String>,
    },
}

impl AgentDispatchStateWire {
    fn into_dispatch(self, legacy_generation: Option<u64>) -> Result<AgentDispatchState, String> {
        match self {
            Self::Open => Ok(AgentDispatchState::Open),
            Self::Awaiting {
                wait: Some(wait),
                wait_id: None,
            } => Ok(AgentDispatchState::Awaiting { wait }),
            Self::Awaiting {
                wait: None,
                wait_id: Some(id),
            } => legacy_generation
                .map(|generation| AgentDispatchState::Awaiting {
                    wait: WaitIdentity { id, generation },
                })
                .ok_or_else(|| {
                    "legacy awaiting dispatch requires an authoritative wait generation".into()
                }),
            Self::Awaiting { .. } => {
                Err("awaiting dispatch requires exactly one of wait or legacy wait_id".into())
            }
        }
    }

    fn into_snapshot_dispatch(
        self,
        waits: &BTreeMap<String, WaitRecord>,
    ) -> Result<AgentDispatchState, String> {
        let legacy_generation = match &self {
            Self::Awaiting {
                wait: None,
                wait_id: Some(id),
            } => waits.get(id).map(|record| record.current_generation),
            _ => None,
        };
        self.into_dispatch(legacy_generation)
    }
}

impl<'de> Deserialize<'de> for AgentDispatchState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        AgentDispatchStateWire::deserialize(deserializer)?
            .into_dispatch(None)
            .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitIdentity {
    pub id: String,
    pub generation: u64,
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
    Waiting {
        wait_id: String,
    },
    Yielded {
        continuation: YieldContinuationRecord,
    },
    NeedsSettlement {
        activation_id: String,
    },
    Paused {
        hold_id: String,
    },
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct YieldContinuationRecord {
    pub continuation_id: String,
    pub source_work_item_id: String,
    pub source_generation: u64,
    pub target_work_item_id: String,
    pub target_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationRecord {
    pub owner: SchedulerOwner,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WaitGenerationRecord {
    pub owner: SchedulerOwner,
    pub state: WaitState,
    #[serde(default)]
    pub trigger: Option<WaitTrigger>,
    #[serde(default)]
    pub consuming_activation_id: Option<String>,
}

#[derive(Deserialize)]
struct WaitGenerationRecordWire {
    #[serde(default)]
    owner: Option<SchedulerOwner>,
    #[serde(default)]
    owner_work_item_id: Option<String>,
    state: WaitState,
    #[serde(default)]
    trigger: Option<WaitTrigger>,
    #[serde(default)]
    consuming_activation_id: Option<String>,
}

impl<'de> Deserialize<'de> for WaitGenerationRecord {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = WaitGenerationRecordWire::deserialize(deserializer)?;
        let owner = scheduler_owner_from_wire_fields(wire.owner, wire.owner_work_item_id)
            .map_err(D::Error::custom)?;
        Ok(Self {
            owner,
            state: wire.state,
            trigger: wire.trigger,
            consuming_activation_id: wire.consuming_activation_id,
        })
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchedulerScenarioClass {
    ReducerOnlyCandidates,
    ExactTaskRejoin,
    ExactWaitResume,
    ExplicitlyBoundOperatorInput,
    WorkItemAutonomousContinuation,
    OrdinarySemanticOperatorBinding,
    OperatorInterjection,
    Settlement,
    Delivery,
}

impl SchedulerScenarioClass {
    pub const PRODUCTION_AUTHORITY: [Self; 8] = [
        Self::ReducerOnlyCandidates,
        Self::WorkItemAutonomousContinuation,
        Self::ExactTaskRejoin,
        Self::ExactWaitResume,
        Self::ExplicitlyBoundOperatorInput,
        Self::Settlement,
        Self::OperatorInterjection,
        Self::Delivery,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReducerOnlyCandidates => "reducer_only_candidates",
            Self::ExactTaskRejoin => "exact_task_rejoin",
            Self::ExactWaitResume => "exact_wait_resume",
            Self::ExplicitlyBoundOperatorInput => "explicitly_bound_operator_input",
            Self::WorkItemAutonomousContinuation => "work_item_autonomous_continuation",
            Self::OrdinarySemanticOperatorBinding => "ordinary_semantic_operator_binding",
            Self::OperatorInterjection => "operator_interjection",
            Self::Settlement => "settlement",
            Self::Delivery => "delivery",
        }
    }

    pub const fn authoritative_dependencies(self) -> &'static [Self] {
        match self {
            Self::OperatorInterjection => &[
                Self::WorkItemAutonomousContinuation,
                Self::ExactTaskRejoin,
                Self::ExactWaitResume,
                Self::ExplicitlyBoundOperatorInput,
                Self::Settlement,
            ],
            _ => &[],
        }
    }
}

impl std::str::FromStr for SchedulerScenarioClass {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "reducer_only_candidates" => Ok(Self::ReducerOnlyCandidates),
            "exact_task_rejoin" => Ok(Self::ExactTaskRejoin),
            "exact_wait_resume" => Ok(Self::ExactWaitResume),
            "explicitly_bound_operator_input" => Ok(Self::ExplicitlyBoundOperatorInput),
            "work_item_autonomous_continuation" => Ok(Self::WorkItemAutonomousContinuation),
            "ordinary_semantic_operator_binding" => Ok(Self::OrdinarySemanticOperatorBinding),
            "operator_interjection" => Ok(Self::OperatorInterjection),
            "settlement" => Ok(Self::Settlement),
            "delivery" => Ok(Self::Delivery),
            _ => Err(()),
        }
    }
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
pub struct AgentActivation {
    pub id: String,
    pub agent_id: String,
    pub state: ActivationLifecycleState,
    pub cause: ActivationCause,
    pub binding: ActivationBinding,
    pub priority: ActivationPriority,
    pub preemption: PreemptionPolicy,
    #[serde(default)]
    pub source_revision: Option<u64>,
    pub idempotency_key: String,
    pub provenance: ActivationProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationAdmissionAuthority {
    pub authority_id: String,
    pub activation: AgentActivation,
    pub expected_scheduling_generation: u64,
    pub expected_dispatch_revision: u64,
    #[serde(default)]
    pub consumed_by: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationLifecycleState {
    Admitted,
    Running,
    Settled,
    Interrupted,
    Cancelled,
    SettlementMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationProvenance {
    pub origin: ActivationOrigin,
    pub trust: ActivationTrust,
    pub source_id: String,
    #[serde(default)]
    pub correlation_id: Option<String>,
    #[serde(default)]
    pub causation_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationOrigin {
    Operator,
    Channel,
    Webhook,
    Callback,
    Timer,
    System,
    Task,
    RuntimeRecovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationTrust {
    OperatorInstruction,
    RuntimeInstruction,
    IntegrationSignal,
    ExternalEvidence,
}

pub fn activation_provenance_has_valid_authority(provenance: &ActivationProvenance) -> bool {
    match provenance.origin {
        ActivationOrigin::Operator => provenance.trust == ActivationTrust::OperatorInstruction,
        ActivationOrigin::Channel | ActivationOrigin::Webhook => matches!(
            provenance.trust,
            ActivationTrust::IntegrationSignal | ActivationTrust::ExternalEvidence
        ),
        ActivationOrigin::Callback => matches!(
            provenance.trust,
            ActivationTrust::RuntimeInstruction
                | ActivationTrust::IntegrationSignal
                | ActivationTrust::ExternalEvidence
        ),
        ActivationOrigin::Timer | ActivationOrigin::System => matches!(
            provenance.trust,
            ActivationTrust::RuntimeInstruction | ActivationTrust::IntegrationSignal
        ),
        ActivationOrigin::Task | ActivationOrigin::RuntimeRecovery => {
            provenance.trust == ActivationTrust::RuntimeInstruction
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivationCause {
    OperatorInput {
        message_id: String,
        #[serde(default)]
        resume: Option<WaitResumeClaim>,
    },
    OperatorInterjection {
        message_id: String,
    },
    MessageIngress {
        message_id: String,
    },
    TaskRejoin {
        task_id: String,
        message_id: String,
        #[serde(default)]
        resume: Option<WaitResumeClaim>,
    },
    WaitResume {
        wait_id: String,
        wait_generation: u64,
        trigger_id: String,
        trigger_generation: u64,
    },
    LifecycleExternalNudge {
        message_id: String,
    },
    WorkItemRunnable {
        work_item_id: String,
        scheduling_generation: u64,
    },
    WorkItemRecheck {
        work_item_id: String,
        recheck_generation: u64,
    },
    InternalFollowup {
        message_id: String,
    },
    RuntimeRecovery {
        recovery_id: String,
    },
    SettlementRecovery {
        activation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitResumeClaim {
    pub wait_id: String,
    pub wait_generation: u64,
    pub trigger_id: String,
    pub trigger_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivationBinding {
    Unbound,
    WorkItem {
        work_item_id: String,
    },
    WaitOwner {
        wait_id: String,
        owner: SchedulerOwner,
    },
    Interaction {
        interaction_id: String,
    },
    Lifecycle {
        agent_id: String,
    },
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ActivationBindingWire {
    Unbound,
    WorkItem {
        work_item_id: String,
    },
    WaitOwner {
        wait_id: String,
        #[serde(default)]
        owner: Option<SchedulerOwner>,
        #[serde(default)]
        owner_work_item_id: Option<String>,
    },
    Interaction {
        interaction_id: String,
    },
    Lifecycle {
        agent_id: String,
    },
}

impl<'de> Deserialize<'de> for ActivationBinding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ActivationBindingWire::deserialize(deserializer)? {
            ActivationBindingWire::Unbound => Ok(Self::Unbound),
            ActivationBindingWire::WorkItem { work_item_id } => Ok(Self::WorkItem { work_item_id }),
            ActivationBindingWire::WaitOwner {
                wait_id,
                owner,
                owner_work_item_id,
            } => Ok(Self::WaitOwner {
                wait_id,
                owner: scheduler_owner_from_wire_fields(owner, owner_work_item_id)
                    .map_err(D::Error::custom)?,
            }),
            ActivationBindingWire::Interaction { interaction_id } => {
                Ok(Self::Interaction { interaction_id })
            }
            ActivationBindingWire::Lifecycle { agent_id } => Ok(Self::Lifecycle { agent_id }),
        }
    }
}

fn scheduler_owner_from_wire_fields(
    owner: Option<SchedulerOwner>,
    owner_work_item_id: Option<String>,
) -> Result<SchedulerOwner, String> {
    match (owner, owner_work_item_id) {
        (Some(owner), None) => Ok(owner),
        (None, Some(work_item_id)) => Ok(SchedulerOwner::WorkItem { work_item_id }),
        (Some(SchedulerOwner::WorkItem { work_item_id }), Some(legacy_work_item_id))
            if work_item_id == legacy_work_item_id =>
        {
            Ok(SchedulerOwner::WorkItem { work_item_id })
        }
        (Some(_), Some(_)) => Err("scheduler owner fields conflict".into()),
        (None, None) => Err("scheduler owner is missing".into()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationPriority {
    Background,
    Normal,
    Next,
    Interject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreemptionPolicy {
    NonPreemptive,
    AllowOperatorInterjection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkDispatchIntent {
    pub work_item_id: String,
    pub scheduling_generation: u64,
    pub class: DispatchClass,
    pub mode: DispatchMode,
    pub priority: ActivationPriority,
    #[serde(default)]
    pub not_before: Option<String>,
    pub state: DispatchState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchClass {
    Start,
    Continue,
    Resume,
    Retry,
    Recheck,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchMode {
    Autonomous,
    OperatorBoundOnly,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DispatchState {
    Offered,
    Reserved { activation_id: String },
    Consumed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivationSettlement {
    pub id: String,
    pub activation_id: String,
    #[serde(default)]
    pub turn_terminal: Option<String>,
    pub disposition: ActivationDisposition,
    pub agent_dispatch: AgentDispatchDisposition,
    #[serde(default)]
    pub operator_delivery: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivationDisposition {
    ConversationReplied,
    WorkContinues,
    WorkWaits {
        wait: WaitIdentity,
    },
    WorkCompleted {
        #[serde(default)]
        continuation: Option<Continuation>,
    },
    WorkPaused {
        reason: String,
    },
    WorkYielded {
        #[serde(default)]
        target_work_item_id: Option<String>,
        #[serde(default)]
        continuation_id: Option<String>,
        #[serde(default)]
        expected_target_generation: Option<u64>,
    },
    WorkFailed {
        failure_policy: String,
    },
    ReducedOnly,
    Interrupted {
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentDispatchDisposition {
    Open,
    Awaiting { wait: WaitIdentity },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmitActivationCommand {
    pub authority_id: String,
    pub activation: AgentActivation,
    pub expected_scheduling_generation: u64,
    pub expected_dispatch_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueActivationAuthorityCommand {
    pub authority_id: String,
    pub activation: AgentActivation,
    pub expected_scheduling_generation: u64,
    pub expected_dispatch_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettleActivationCommand {
    pub settlement: ActivationSettlement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisterWorkDemandCommand {
    pub work_item_id: String,
    pub demand: WorkDemand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyWaitAdoption {
    pub wait_id: String,
    pub generation: u64,
    pub owner_work_item_id: String,
    pub source_updated_at: String,
}

/// Proof that allows an `AdoptLegacyWorkState` command to atomically replace
/// a stale canonical focus whose legacy WorkItem is provably completed.
///
/// This is NOT a general-purpose focus replacement. It only permits
/// terminalizing one old focus demand and switching to the new adoption
/// target in the same reducer transition, within the narrow legacy→canonical
/// migration window described in issue #2460.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceCompletedFocusProof {
    pub work_item_id: String,
    pub source_work_item_revision: u64,
    pub expected_metadata_revision: u64,
    pub expected_scheduling_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptLegacyWorkStateCommand {
    pub work_item_id: String,
    pub source_work_item_revision: u64,
    pub demand: WorkDemand,
    #[serde(default)]
    pub wait: Option<LegacyWaitAdoption>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default)]
    pub reserve_dispatch: bool,
    #[serde(default)]
    pub replace_completed_focus: Option<ReplaceCompletedFocusProof>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyCompletionReport {
    pub turn_terminal: String,
    pub operator_delivery: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyEventMigrationContext {
    pub record_id: String,
    pub agent_id: String,
    pub source_id: String,
    pub recorded_at: String,
    #[serde(default)]
    pub admission_provenance: Option<ActivationProvenance>,
    #[serde(default)]
    pub completion_report: Option<LegacyCompletionReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyEventMigration {
    #[serde(default)]
    pub authority_command: Option<IssueActivationAuthorityCommand>,
    pub command: ProtocolCommand,
    pub outcome: ProtocolCommandOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingSettlementRecord {
    pub id: String,
    pub activation_id: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerWaitCommand {
    pub wait_id: String,
    pub wait_generation: u64,
    pub trigger_id: String,
    pub trigger_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActivationInputAttachment {
    pub id: String,
    pub activation_id: String,
    pub owner: SchedulerOwner,
    pub expected_admitted_generation: u64,
    pub expected_dispatch_revision: u64,
    pub message_id: String,
    pub turn_id: String,
    pub boundary: String,
    pub round: u64,
    pub provenance: ActivationProvenance,
    pub created_at: String,
}

#[derive(Deserialize)]
struct ActivationInputAttachmentWire {
    id: String,
    activation_id: String,
    #[serde(default)]
    owner: Option<SchedulerOwner>,
    #[serde(default)]
    expected_admitted_generation: Option<u64>,
    #[serde(default)]
    work_item_id: Option<String>,
    #[serde(default)]
    expected_scheduling_generation: Option<u64>,
    expected_dispatch_revision: u64,
    message_id: String,
    turn_id: String,
    boundary: String,
    round: u64,
    provenance: ActivationProvenance,
    created_at: String,
}

impl<'de> Deserialize<'de> for ActivationInputAttachment {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ActivationInputAttachmentWire::deserialize(deserializer)?;
        let (owner, expected_admitted_generation) = match (
            wire.owner,
            wire.expected_admitted_generation,
            wire.work_item_id,
            wire.expected_scheduling_generation,
        ) {
            (Some(owner), Some(generation), None, None) => (owner, generation),
            (None, None, Some(work_item_id), Some(generation)) => {
                (SchedulerOwner::WorkItem { work_item_id }, generation)
            }
            _ => {
                return Err(D::Error::custom(
                    "activation input requires exactly one complete owner/generation format",
                ));
            }
        };
        Ok(Self {
            id: wire.id,
            activation_id: wire.activation_id,
            owner,
            expected_admitted_generation,
            expected_dispatch_revision: wire.expected_dispatch_revision,
            message_id: wire.message_id,
            turn_id: wire.turn_id,
            boundary: wire.boundary,
            round: wire.round,
            provenance: wire.provenance,
            created_at: wire.created_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachActivationInputCommand {
    pub attachment: ActivationInputAttachment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProtocolCommand {
    RegisterWorkDemand(RegisterWorkDemandCommand),
    AdoptLegacyWorkState(AdoptLegacyWorkStateCommand),
    IssueActivationAuthority(IssueActivationAuthorityCommand),
    AdmitActivation(AdmitActivationCommand),
    SettleActivation(SettleActivationCommand),
    RecordMissingSettlement(MissingSettlementRecord),
    TriggerWait(TriggerWaitCommand),
    AttachActivationInput(AttachActivationInputCommand),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RolloutCommand {
    ConfigureProtocol {
        expected_config_revision: u64,
        mode: ProtocolMode,
    },
    OpenPreflight {
        expected_config_revision: u64,
        manifest_revision: u64,
    },
    CompletePreflight {
        expected_config_revision: u64,
        expected_preflight_revision: u64,
        manifest: RolloutManifest,
    },
    InstallManifest {
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
    ChangeScenarioAuthorityFromExplicitMode {
        scenario_class: String,
        expected_config_revision: u64,
        expected_manifest_revision: u64,
        expected_preflight_revision: u64,
    },
    ReportScenarioHardBlocker {
        scenario_class: String,
        blocker_code: String,
        expected_config_revision: u64,
        expected_manifest_revision: u64,
        expected_preflight_revision: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolConflictKind {
    InvalidCommand,
    UnsupportedCommand,
    AuthorityConflict,
    IdentityConflict,
    IdempotencyConflict,
    PayloadConflict,
    BindingConflict,
    StaleRevision,
    StaleGeneration,
    Duplicate,
    NotFound,
    StateConflict,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolConflict {
    pub kind: ProtocolConflictKind,
    pub code: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolCommandOutcome {
    pub outcome: Outcome,
    #[serde(default)]
    pub conflict: Option<ProtocolConflict>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Admit {
        activation_id: String,
        owner: SchedulerOwner,
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
    ChangeScenarioAuthorityFromExplicitMode {
        scenario_class: String,
        expected_config_revision: u64,
        expected_manifest_revision: u64,
        expected_preflight_revision: u64,
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
    AttachActivationInput {
        attachment: ActivationInputAttachment,
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
    TaskRejoin {
        task_id: String,
        message_id: String,
        #[serde(default)]
        resume: Option<WaitResumeClaim>,
    },
    OperatorInput {
        message_id: String,
        #[serde(default)]
        resume: Option<WaitResumeClaim>,
    },
    WaitResume {
        wait_id: String,
        wait_generation: u64,
        trigger_id: String,
        trigger_generation: u64,
    },
    LifecycleExternalNudge {
        message_id: String,
    },
    SettlementRecovery {
        missing_activation_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Settlement {
    Continue,
    Yield,
    TargetedYield {
        continuation: YieldContinuationRecord,
    },
    Wait {
        wait: WaitIdentity,
        mode: WaitMode,
        legacy_wait_id: bool,
    },
    Complete {
        continuation: Option<Continuation>,
    },
    Missing,
}

impl Serialize for Settlement {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire<'a> {
            Continue,
            Yield {
                #[serde(skip_serializing_if = "Option::is_none")]
                continuation: Option<&'a YieldContinuationRecord>,
            },
            Wait {
                #[serde(skip_serializing_if = "Option::is_none")]
                wait: Option<&'a WaitIdentity>,
                #[serde(skip_serializing_if = "Option::is_none")]
                wait_id: Option<&'a str>,
                mode: WaitMode,
            },
            Complete {
                continuation: &'a Option<Continuation>,
            },
            Missing,
        }

        let wire = match self {
            Self::Continue => Wire::Continue,
            Self::Yield => Wire::Yield { continuation: None },
            Self::TargetedYield { continuation } => Wire::Yield {
                continuation: Some(continuation),
            },
            Self::Wait {
                wait,
                mode,
                legacy_wait_id,
            } => Wire::Wait {
                wait: (!legacy_wait_id).then_some(wait),
                wait_id: legacy_wait_id.then_some(wait.id.as_str()),
                mode: *mode,
            },
            Self::Complete { continuation } => Wire::Complete { continuation },
            Self::Missing => Wire::Missing,
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Settlement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum Wire {
            Continue,
            Yield {
                #[serde(default)]
                continuation: Option<YieldContinuationRecord>,
            },
            Wait {
                #[serde(default)]
                wait: Option<WaitIdentity>,
                #[serde(default)]
                wait_id: Option<String>,
                mode: WaitMode,
            },
            Complete {
                #[serde(default)]
                continuation: Option<Continuation>,
            },
            Missing,
        }

        match Wire::deserialize(deserializer)? {
            Wire::Continue => Ok(Self::Continue),
            Wire::Yield { continuation: None } => Ok(Self::Yield),
            Wire::Yield {
                continuation: Some(continuation),
            } => Ok(Self::TargetedYield { continuation }),
            Wire::Wait {
                wait: Some(wait),
                wait_id: None,
                mode,
            } => Ok(Self::Wait {
                wait,
                mode,
                legacy_wait_id: false,
            }),
            Wire::Wait {
                wait: None,
                wait_id: Some(id),
                mode,
            } => Ok(Self::Wait {
                wait: WaitIdentity { id, generation: 0 },
                mode,
                legacy_wait_id: true,
            }),
            Wire::Wait { .. } => Err(D::Error::custom(
                "wait settlement requires exactly one of wait or legacy wait_id",
            )),
            Wire::Complete { continuation } => Ok(Self::Complete { continuation }),
            Wire::Missing => Ok(Self::Missing),
        }
    }
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
    pub expected_caller_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuationAdmissionRecord {
    pub admission_id: String,
    pub settlement_id: String,
    pub completed_work_item_id: String,
    pub caller_work_item_id: String,
    pub expected_caller_generation: u64,
    pub expected_caller_status: WorkStatus,
    pub admitted_caller_generation: u64,
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
    WorkDemandRegistered,
    LegacyWorkStateAdopted,
    AuthorityIssued,
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
    ActivationInputAttached,
    SettlementMissing,
    SettlementHeld,
    Rejected,
}

pub fn reduce(snapshot: &Snapshot, event: &Event) -> Outcome {
    if matches!(event, Event::Admit { .. } | Event::Settle { .. }) {
        return rejected(snapshot, "typed_protocol_command_required");
    }
    reduce_event(snapshot, event)
}

pub fn reduce_rollout_command(
    snapshot: &Snapshot,
    command: &RolloutCommand,
) -> ProtocolCommandOutcome {
    let event = match command {
        RolloutCommand::ConfigureProtocol {
            expected_config_revision,
            mode,
        } => Event::ConfigureProtocol {
            expected_config_revision: *expected_config_revision,
            mode: *mode,
        },
        RolloutCommand::OpenPreflight {
            expected_config_revision,
            manifest_revision,
        } => Event::OpenRolloutPreflight {
            expected_config_revision: *expected_config_revision,
            manifest_revision: *manifest_revision,
        },
        RolloutCommand::CompletePreflight {
            expected_config_revision,
            expected_preflight_revision,
            manifest,
        } => Event::CompleteRolloutPreflight {
            expected_config_revision: *expected_config_revision,
            expected_preflight_revision: *expected_preflight_revision,
            manifest: manifest.clone(),
        },
        RolloutCommand::InstallManifest {
            expected_config_revision,
            manifest,
        } => Event::InstallRolloutManifest {
            expected_config_revision: *expected_config_revision,
            manifest: manifest.clone(),
        },
        RolloutCommand::ChangeScenarioAuthority {
            scenario_class,
            expected_config_revision,
            expected_manifest_revision,
            expected_preflight_revision,
            mode,
        } => Event::ChangeScenarioAuthority {
            scenario_class: scenario_class.clone(),
            expected_config_revision: *expected_config_revision,
            expected_manifest_revision: *expected_manifest_revision,
            expected_preflight_revision: *expected_preflight_revision,
            mode: *mode,
        },
        RolloutCommand::ChangeScenarioAuthorityFromExplicitMode {
            scenario_class,
            expected_config_revision,
            expected_manifest_revision,
            expected_preflight_revision,
        } => Event::ChangeScenarioAuthorityFromExplicitMode {
            scenario_class: scenario_class.clone(),
            expected_config_revision: *expected_config_revision,
            expected_manifest_revision: *expected_manifest_revision,
            expected_preflight_revision: *expected_preflight_revision,
        },
        RolloutCommand::ReportScenarioHardBlocker {
            scenario_class,
            blocker_code,
            expected_config_revision,
            expected_manifest_revision,
            expected_preflight_revision,
        } => Event::ReportScenarioHardBlocker {
            scenario_class: scenario_class.clone(),
            blocker_code: blocker_code.clone(),
            expected_config_revision: *expected_config_revision,
            expected_manifest_revision: *expected_manifest_revision,
            expected_preflight_revision: *expected_preflight_revision,
        },
    };
    let outcome = reduce_event(snapshot, &event);
    let conflict =
        (outcome.decision == Decision::Rejected).then(|| reducer_conflict(&outcome.diagnostics[0]));
    ProtocolCommandOutcome { outcome, conflict }
}

pub fn migrate_legacy_event(
    snapshot: &Snapshot,
    event: &Event,
    context: &LegacyEventMigrationContext,
) -> Result<LegacyEventMigration, ProtocolConflict> {
    if context.record_id.is_empty()
        || context.agent_id.is_empty()
        || context.source_id.is_empty()
        || context.recorded_at.is_empty()
    {
        return Err(command_conflict(
            ProtocolConflictKind::InvalidCommand,
            "legacy_migration_context_required",
        ));
    }

    let (command, authority_command) = match event {
        Event::Admit {
            activation_id,
            owner,
            expected_generation,
            expected_dispatch_revision,
            cause,
        } => {
            let SchedulerOwner::WorkItem { work_item_id } = owner else {
                return Err(command_conflict(
                    ProtocolConflictKind::UnsupportedCommand,
                    "legacy_lifecycle_admission_migration_unsupported",
                ));
            };
            let provenance = context.admission_provenance.clone().ok_or_else(|| {
                command_conflict(
                    ProtocolConflictKind::InvalidCommand,
                    "legacy_admission_provenance_required",
                )
            })?;
            if provenance.source_id != context.source_id {
                return Err(command_conflict(
                    ProtocolConflictKind::PayloadConflict,
                    "legacy_admission_source_identity_mismatch",
                ));
            }
            let (typed_cause, binding) = match cause {
                AdmissionCause::Scheduling => (
                    ActivationCause::WorkItemRunnable {
                        work_item_id: work_item_id.clone(),
                        scheduling_generation: *expected_generation,
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
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
                ),
                AdmissionCause::OperatorInput { message_id, resume } => (
                    ActivationCause::OperatorInput {
                        message_id: message_id.clone(),
                        resume: resume.clone(),
                    },
                    ActivationBinding::WorkItem {
                        work_item_id: work_item_id.clone(),
                    },
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
                ),
                AdmissionCause::LifecycleExternalNudge { .. } => {
                    return Err(command_conflict(
                        ProtocolConflictKind::UnsupportedCommand,
                        "legacy_lifecycle_admission_migration_unsupported",
                    ));
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
                ),
            };
            let command = AdmitActivationCommand {
                authority_id: format!("legacy-authority:{}", context.record_id),
                activation: AgentActivation {
                    id: activation_id.clone(),
                    agent_id: context.agent_id.clone(),
                    state: ActivationLifecycleState::Admitted,
                    cause: typed_cause,
                    binding,
                    priority: ActivationPriority::Normal,
                    preemption: PreemptionPolicy::NonPreemptive,
                    source_revision: None,
                    idempotency_key: format!("legacy-event:{}", context.record_id),
                    provenance,
                },
                expected_scheduling_generation: *expected_generation,
                expected_dispatch_revision: *expected_dispatch_revision,
            };
            let authority_command = IssueActivationAuthorityCommand {
                authority_id: command.authority_id.clone(),
                activation: command.activation.clone(),
                expected_scheduling_generation: command.expected_scheduling_generation,
                expected_dispatch_revision: command.expected_dispatch_revision,
            };
            (
                ProtocolCommand::AdmitActivation(command),
                Some(authority_command),
            )
        }
        Event::Settle {
            activation_id,
            settlement: Settlement::Missing,
        } => (
            ProtocolCommand::RecordMissingSettlement(MissingSettlementRecord {
                id: context.record_id.clone(),
                activation_id: activation_id.clone(),
                created_at: context.recorded_at.clone(),
            }),
            None,
        ),
        Event::Settle {
            activation_id,
            settlement,
        } => {
            let admitted_generation = snapshot
                .activations
                .get(activation_id)
                .ok_or_else(|| {
                    command_conflict(
                        ProtocolConflictKind::NotFound,
                        "settlement_activation_missing",
                    )
                })?
                .admitted_generation;
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
                            WaitMode::AwaitThis => AgentDispatchDisposition::Awaiting { wait },
                            WaitMode::AcceptScheduling => AgentDispatchDisposition::Open,
                        },
                    )
                }
                Settlement::Complete { continuation } => (
                    ActivationDisposition::WorkCompleted {
                        continuation: continuation.clone(),
                    },
                    AgentDispatchDisposition::Open,
                ),
                Settlement::Missing => unreachable!("handled above"),
            };
            let completion = matches!(disposition, ActivationDisposition::WorkCompleted { .. });
            let report = context.completion_report.as_ref();
            if completion && report.is_none() {
                return Err(command_conflict(
                    ProtocolConflictKind::InvalidCommand,
                    "legacy_completion_report_required",
                ));
            }
            (
                ProtocolCommand::SettleActivation(SettleActivationCommand {
                    settlement: ActivationSettlement {
                        id: context.record_id.clone(),
                        activation_id: activation_id.clone(),
                        turn_terminal: report.map(|report| report.turn_terminal.clone()),
                        disposition,
                        agent_dispatch,
                        operator_delivery: report.map(|report| report.operator_delivery.clone()),
                        evidence: report
                            .map(|report| report.evidence.clone())
                            .unwrap_or_default(),
                        created_at: context.recorded_at.clone(),
                    },
                }),
                None,
            )
        }
        _ => {
            return Err(command_conflict(
                ProtocolConflictKind::UnsupportedCommand,
                "event_is_not_legacy_protocol_boundary",
            ));
        }
    };

    let authorized = if let Some(authority_command) = &authority_command {
        let issued = reduce_command(
            snapshot,
            &ProtocolCommand::IssueActivationAuthority(authority_command.clone()),
        );
        if issued.outcome.decision == Decision::Rejected {
            return Err(issued.conflict.unwrap_or_else(|| {
                command_conflict(
                    ProtocolConflictKind::AuthorityConflict,
                    "legacy_migration_authority_conflict",
                )
            }));
        }
        issued.outcome.snapshot
    } else {
        snapshot.clone()
    };
    let mut outcome = reduce_command(&authorized, &command);
    if outcome.outcome.decision == Decision::Rejected {
        outcome.outcome.snapshot = snapshot.clone();
    }
    Ok(LegacyEventMigration {
        authority_command,
        command,
        outcome,
    })
}

fn reduce_event(snapshot: &Snapshot, event: &Event) -> Outcome {
    match event {
        Event::Admit {
            activation_id,
            owner,
            expected_generation,
            expected_dispatch_revision,
            cause,
        } => admit(
            snapshot,
            activation_id,
            owner,
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
            false,
        ),
        Event::ChangeScenarioAuthorityFromExplicitMode {
            scenario_class,
            expected_config_revision,
            expected_manifest_revision,
            expected_preflight_revision,
        } => change_scenario_authority(
            snapshot,
            scenario_class,
            *expected_config_revision,
            *expected_manifest_revision,
            *expected_preflight_revision,
            ScenarioMode::Authoritative,
            true,
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
        Event::AttachActivationInput { attachment } => {
            attach_activation_input(snapshot, attachment)
        }
        Event::Settle {
            activation_id,
            settlement,
        } => settle(snapshot, activation_id, settlement),
    }
}

pub fn reduce_command(snapshot: &Snapshot, command: &ProtocolCommand) -> ProtocolCommandOutcome {
    if let Some(outcome) = replay_or_conflict(snapshot, command) {
        return outcome;
    }
    if let ProtocolCommand::RegisterWorkDemand(command) = command {
        return register_work_demand(snapshot, command);
    }
    if let ProtocolCommand::AdoptLegacyWorkState(command) = command {
        return adopt_legacy_work_state(snapshot, command);
    }
    if let ProtocolCommand::IssueActivationAuthority(command) = command {
        return issue_activation_authority(snapshot, command);
    }
    let event = match lower_command(snapshot, command) {
        Ok(event) => event,
        Err(conflict) => {
            return rejected_command(snapshot, conflict);
        }
    };
    let mut outcome = reduce_event(snapshot, &event);
    match (command, &outcome.decision) {
        (ProtocolCommand::AdmitActivation(command), Decision::Admitted) => {
            outcome
                .snapshot
                .activation_authorities
                .get_mut(&command.authority_id)
                .expect("validated activation authority exists")
                .consumed_by = Some(command.activation.id.clone());
            outcome
                .snapshot
                .activation_admissions
                .insert(command.activation.id.clone(), command.clone());
        }
        (ProtocolCommand::SettleActivation(command), Decision::Settled) => {
            outcome
                .snapshot
                .settlements
                .insert(command.settlement.id.clone(), command.settlement.clone());
            if let ActivationDisposition::WorkCompleted {
                continuation: Some(continuation),
            } = &command.settlement.disposition
            {
                let activation = snapshot
                    .activations
                    .get(&command.settlement.activation_id)
                    .expect("validated settlement activation exists");
                let completed_work_item_id = activation
                    .owner
                    .work_item_id()
                    .expect("completion continuation requires a WorkItem owner");
                outcome.snapshot.continuation_admissions.insert(
                    continuation.admission_id.clone(),
                    ContinuationAdmissionRecord {
                        admission_id: continuation.admission_id.clone(),
                        settlement_id: command.settlement.id.clone(),
                        completed_work_item_id: completed_work_item_id.to_string(),
                        caller_work_item_id: continuation.caller_work_item_id.clone(),
                        expected_caller_generation: continuation.expected_caller_generation,
                        expected_caller_status: WorkStatus::Runnable,
                        admitted_caller_generation: continuation.expected_caller_generation + 1,
                    },
                );
            }
        }
        (
            ProtocolCommand::RecordMissingSettlement(record),
            Decision::SettlementMissing | Decision::SettlementHeld,
        ) => {
            outcome
                .snapshot
                .missing_settlements
                .insert(record.id.clone(), record.clone());
        }
        (ProtocolCommand::AttachActivationInput(command), Decision::ActivationInputAttached) => {
            outcome
                .snapshot
                .activation_inputs
                .insert(command.attachment.id.clone(), command.attachment.clone());
        }
        _ => {}
    }
    let conflict = (outcome.decision == Decision::Rejected).then(|| {
        reducer_conflict(
            outcome
                .diagnostics
                .first()
                .map(String::as_str)
                .unwrap_or("rejected_without_diagnostic"),
        )
    });
    ProtocolCommandOutcome { outcome, conflict }
}

fn replay_or_conflict(
    snapshot: &Snapshot,
    command: &ProtocolCommand,
) -> Option<ProtocolCommandOutcome> {
    match command {
        ProtocolCommand::RegisterWorkDemand(command) => {
            if let Some(existing) = snapshot.work.get(&command.work_item_id) {
                return Some(if existing == &command.demand {
                    duplicate_command(snapshot, "work_demand_already_registered")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::IdentityConflict,
                            "work_demand_registration_conflict",
                        ),
                    )
                });
            }
        }
        ProtocolCommand::AdoptLegacyWorkState(command) => {
            if let Some(existing) = snapshot.work.get(&command.work_item_id) {
                if existing.metadata_revision != command.source_work_item_revision {
                    return None;
                }
                let wait_matches = match &command.wait {
                    Some(wait) => snapshot.waits.get(&wait.wait_id).is_some_and(|existing| {
                        existing.current_generation == wait.generation
                            && existing.generations.get(&wait.generation)
                                == Some(&WaitGenerationRecord {
                                    owner: SchedulerOwner::WorkItem {
                                        work_item_id: wait.owner_work_item_id.clone(),
                                    },
                                    state: WaitState::Active,
                                    trigger: None,
                                    consuming_activation_id: None,
                                })
                    }),
                    None => true,
                };
                let focus_matches = !command.focus
                    || snapshot.focus.as_deref() == Some(command.work_item_id.as_str())
                    || command
                        .replace_completed_focus
                        .as_ref()
                        .is_some_and(|proof| {
                            snapshot.focus.as_deref() == Some(proof.work_item_id.as_str())
                        });
                let dispatch_matches = !command.reserve_dispatch
                    || command.wait.as_ref().is_some_and(|wait| {
                        snapshot.dispatch
                            == (AgentDispatchState::Awaiting {
                                wait: WaitIdentity {
                                    id: wait.wait_id.clone(),
                                    generation: wait.generation,
                                },
                            })
                    });
                return Some(
                    if existing == &command.demand
                        && wait_matches
                        && focus_matches
                        && dispatch_matches
                    {
                        duplicate_command(snapshot, "legacy_work_state_already_adopted")
                    } else {
                        rejected_command(
                            snapshot,
                            command_conflict(
                                ProtocolConflictKind::IdentityConflict,
                                "legacy_work_state_adoption_conflict",
                            ),
                        )
                    },
                );
            }
        }
        ProtocolCommand::IssueActivationAuthority(command) => {
            if let Some(existing) = snapshot.activation_authorities.get(&command.authority_id) {
                return Some(if authority_matches_issue(existing, command) {
                    duplicate_command(snapshot, "activation_authority_already_issued")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::AuthorityConflict,
                            "activation_authority_id_command_conflict",
                        ),
                    )
                });
            }
        }
        ProtocolCommand::AdmitActivation(command) => {
            if let Some(existing) = snapshot.activation_admissions.get(&command.activation.id) {
                return Some(if existing == command {
                    duplicate_command(snapshot, "activation_command_already_applied")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::IdentityConflict,
                            "activation_id_command_conflict",
                        ),
                    )
                });
            }
            if snapshot.activation_admissions.values().any(|existing| {
                existing.activation.idempotency_key == command.activation.idempotency_key
            }) {
                return Some(rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::IdempotencyConflict,
                        "activation_idempotency_key_conflict",
                    ),
                ));
            }
        }
        ProtocolCommand::SettleActivation(command) => {
            if let Some(existing) = snapshot.settlements.get(&command.settlement.id) {
                return Some(if existing == &command.settlement {
                    duplicate_command(snapshot, "settlement_command_already_applied")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::IdentityConflict,
                            "settlement_id_command_conflict",
                        ),
                    )
                });
            }
            if snapshot
                .settlements
                .values()
                .any(|existing| existing.activation_id == command.settlement.activation_id)
            {
                return Some(rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::StateConflict,
                        "activation_terminal_settlement_already_recorded",
                    ),
                ));
            }
        }
        ProtocolCommand::RecordMissingSettlement(record) => {
            if let Some(existing) = snapshot.missing_settlements.get(&record.id) {
                return Some(if existing == record {
                    duplicate_command(snapshot, "missing_settlement_command_already_applied")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::IdentityConflict,
                            "missing_settlement_id_command_conflict",
                        ),
                    )
                });
            }
            if snapshot
                .missing_settlements
                .values()
                .any(|existing| existing.activation_id == record.activation_id)
            {
                return Some(rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::StateConflict,
                        "activation_missing_settlement_already_recorded",
                    ),
                ));
            }
        }
        ProtocolCommand::TriggerWait(_) => {}
        ProtocolCommand::AttachActivationInput(command) => {
            if let Some(existing) = snapshot.activation_inputs.get(&command.attachment.id) {
                return Some(if existing == &command.attachment {
                    duplicate_command(snapshot, "activation_input_already_attached")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::IdentityConflict,
                            "activation_input_id_command_conflict",
                        ),
                    )
                });
            }
            if snapshot
                .activation_inputs
                .values()
                .any(|existing| existing.message_id == command.attachment.message_id)
            {
                return Some(rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::IdempotencyConflict,
                        "activation_input_message_conflict",
                    ),
                ));
            }
        }
    }
    None
}

fn duplicate_command(snapshot: &Snapshot, diagnostic: &str) -> ProtocolCommandOutcome {
    ProtocolCommandOutcome {
        outcome: Outcome {
            decision: Decision::DuplicateIgnored,
            transitions: Vec::new(),
            diagnostics: vec![diagnostic.to_string()],
            snapshot: snapshot.clone(),
        },
        conflict: None,
    }
}

fn rejected_command(snapshot: &Snapshot, conflict: ProtocolConflict) -> ProtocolCommandOutcome {
    ProtocolCommandOutcome {
        outcome: rejected(snapshot, &conflict.code),
        conflict: Some(conflict),
    }
}

fn lower_command(
    snapshot: &Snapshot,
    command: &ProtocolCommand,
) -> Result<Event, ProtocolConflict> {
    match command {
        ProtocolCommand::RegisterWorkDemand(_) | ProtocolCommand::AdoptLegacyWorkState(_) => {
            unreachable!("work demand mutations are reduced directly")
        }
        ProtocolCommand::IssueActivationAuthority(_) => {
            unreachable!("authority issuance is reduced directly")
        }
        ProtocolCommand::AdmitActivation(command) => {
            lower_admit_activation(snapshot, command, false)
        }
        ProtocolCommand::SettleActivation(command) => {
            if !snapshot
                .activation_admissions
                .contains_key(&command.settlement.activation_id)
            {
                return Err(command_conflict(
                    ProtocolConflictKind::AuthorityConflict,
                    "activation_has_no_canonical_admission",
                ));
            }
            let event = lower_activation_settlement(snapshot, command)?;
            if let ActivationDisposition::WorkCompleted {
                continuation: Some(continuation),
            } = &command.settlement.disposition
            {
                let activation = snapshot
                    .activations
                    .get(&command.settlement.activation_id)
                    .expect("validated settlement activation exists");
                let work_item_id = activation.owner.work_item_id().ok_or_else(|| {
                    command_conflict(
                        ProtocolConflictKind::BindingConflict,
                        "continuation_requires_work_item_owner",
                    )
                })?;
                validate_continuation_target(snapshot, work_item_id, continuation)
                    .map_err(reducer_conflict)?;
            }
            Ok(event)
        }
        ProtocolCommand::RecordMissingSettlement(record) => {
            if !snapshot
                .activation_admissions
                .contains_key(&record.activation_id)
            {
                return Err(command_conflict(
                    ProtocolConflictKind::AuthorityConflict,
                    "activation_has_no_canonical_admission",
                ));
            }
            if record.id.is_empty()
                || record.activation_id.is_empty()
                || record.created_at.is_empty()
            {
                return Err(command_conflict(
                    ProtocolConflictKind::InvalidCommand,
                    "missing_settlement_identity_required",
                ));
            }
            Ok(Event::Settle {
                activation_id: record.activation_id.clone(),
                settlement: Settlement::Missing,
            })
        }
        ProtocolCommand::TriggerWait(command) => {
            if command.wait_id.is_empty() || command.trigger_id.is_empty() {
                return Err(command_conflict(
                    ProtocolConflictKind::InvalidCommand,
                    "wait_and_trigger_identity_required",
                ));
            }
            Ok(Event::TriggerWait {
                wait_id: command.wait_id.clone(),
                wait_generation: command.wait_generation,
                trigger_id: command.trigger_id.clone(),
                trigger_generation: command.trigger_generation,
            })
        }
        ProtocolCommand::AttachActivationInput(command) => {
            let attachment = &command.attachment;
            if attachment.id.is_empty()
                || attachment.activation_id.is_empty()
                || match &attachment.owner {
                    SchedulerOwner::WorkItem { work_item_id } => work_item_id.is_empty(),
                    SchedulerOwner::AgentLifecycle { agent_id } => agent_id.is_empty(),
                }
                || attachment.expected_admitted_generation == 0
                || attachment.message_id.is_empty()
                || attachment.turn_id.is_empty()
                || attachment.boundary.is_empty()
                || attachment.created_at.is_empty()
                || attachment.provenance.source_id != attachment.message_id
                || attachment.provenance.origin != ActivationOrigin::Operator
                || attachment.provenance.trust != ActivationTrust::OperatorInstruction
            {
                return Err(command_conflict(
                    ProtocolConflictKind::InvalidCommand,
                    "activation_input_identity_or_provenance_required",
                ));
            }
            Ok(Event::AttachActivationInput {
                attachment: attachment.clone(),
            })
        }
    }
}

fn register_work_demand(
    snapshot: &Snapshot,
    command: &RegisterWorkDemandCommand,
) -> ProtocolCommandOutcome {
    if command.work_item_id.is_empty()
        || command.demand.metadata_revision == 0
        || command.demand.scheduling_generation == 0
        || command.demand.locality.is_empty()
        || command.demand.cost_class.is_empty()
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "work_demand_registration_fields_required",
            ),
        );
    }
    if command.demand.status != WorkStatus::Runnable {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::UnsupportedCommand,
                "initial_work_demand_must_be_runnable",
            ),
        );
    }

    let mut next = snapshot.clone();
    next.work
        .insert(command.work_item_id.clone(), command.demand.clone());
    ProtocolCommandOutcome {
        outcome: Outcome {
            decision: Decision::WorkDemandRegistered,
            transitions: vec![format!(
                "work:{}:registered:generation:{}",
                command.work_item_id, command.demand.scheduling_generation
            )],
            diagnostics: Vec::new(),
            snapshot: next,
        },
        conflict: None,
    }
}

fn adopt_legacy_work_state(
    snapshot: &Snapshot,
    command: &AdoptLegacyWorkStateCommand,
) -> ProtocolCommandOutcome {
    if command.work_item_id.is_empty()
        || command.source_work_item_revision == 0
        || command.demand.metadata_revision != command.source_work_item_revision
        || command.demand.scheduling_generation == 0
        || command.demand.locality.is_empty()
        || command.demand.cost_class.is_empty()
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "legacy_work_state_adoption_fields_required",
            ),
        );
    }
    match (&command.demand.status, &command.wait) {
        (WorkStatus::Runnable, None) => {}
        (WorkStatus::Waiting { wait_id }, Some(wait))
            if wait_id == &wait.wait_id
                && wait.owner_work_item_id == command.work_item_id
                && wait.generation == command.demand.scheduling_generation
                && !wait.source_updated_at.is_empty() => {}
        (WorkStatus::Yielded { .. } | WorkStatus::Paused { .. }, None) => {}
        _ => {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::UnsupportedCommand,
                    "legacy_work_state_adoption_shape_unsupported",
                ),
            );
        }
    }
    if command.reserve_dispatch && command.wait.is_none() {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "legacy_dispatch_adoption_requires_wait",
            ),
        );
    }
    if command.focus
        && snapshot
            .focus
            .as_deref()
            .is_some_and(|focus| focus != command.work_item_id)
    {
        // Without a replacement proof, any focus mismatch is a hard conflict.
        let Some(proof) = &command.replace_completed_focus else {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "legacy_focus_adoption_conflict",
                ),
            );
        };
        // Current focus must match the proof's work item.
        if snapshot.focus.as_deref() != Some(proof.work_item_id.as_str()) {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "legacy_focus_replacement_focus_mismatch",
                ),
            );
        }
        // Old focus demand must exist and match the proof fence.
        let old_demand = match snapshot.work.get(&proof.work_item_id) {
            Some(demand)
                if demand.metadata_revision == proof.expected_metadata_revision
                    && demand.scheduling_generation == proof.expected_scheduling_generation =>
            {
                demand
            }
            _ => {
                return rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::StaleRevision,
                        "legacy_focus_replacement_demand_fence_mismatch",
                    ),
                );
            }
        };
        // Old focus must be in a safe state: Runnable or Paused only.
        if !matches!(
            old_demand.status,
            WorkStatus::Runnable | WorkStatus::Paused { .. }
        ) {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "legacy_focus_replacement_unsafe_status",
                ),
            );
        }
        // Activation slot must not be occupied by the old focus.
        if let ActivationSlot::Running { owner, .. } = &snapshot.slot {
            if owner.work_item_id() == Some(proof.work_item_id.as_str()) {
                return rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::StateConflict,
                        "legacy_focus_replacement_slot_occupied",
                    ),
                );
            }
        }
    }
    if command.reserve_dispatch && snapshot.dispatch != AgentDispatchState::Open {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::StateConflict,
                "legacy_dispatch_adoption_conflict",
            ),
        );
    }
    if snapshot
        .work
        .get(&command.work_item_id)
        .is_some_and(|existing| {
            existing.metadata_revision > command.source_work_item_revision
                || matches!(
                    existing.status,
                    WorkStatus::NeedsSettlement { .. } | WorkStatus::Terminal
                )
        })
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::StaleRevision,
                "legacy_work_state_adoption_stale",
            ),
        );
    }

    let mut next = snapshot.clone();
    if let Some(existing) = next.work.get(&command.work_item_id) {
        if let WorkStatus::Waiting { wait_id } = &existing.status {
            if command.wait.as_ref().map(|wait| wait.wait_id.as_str()) != Some(wait_id.as_str()) {
                if let Some(wait) = next.waits.get_mut(wait_id) {
                    if let Some(generation) = wait.generations.get_mut(&wait.current_generation) {
                        if matches!(generation.state, WaitState::Active | WaitState::Triggered) {
                            generation.state = WaitState::Resolved;
                            generation.trigger = None;
                        }
                    }
                }
            }
        }
    }
    next.work
        .insert(command.work_item_id.clone(), command.demand.clone());
    if let Some(wait) = &command.wait {
        next.waits.insert(
            wait.wait_id.clone(),
            WaitRecord {
                current_generation: wait.generation,
                generations: BTreeMap::from([(
                    wait.generation,
                    WaitGenerationRecord {
                        owner: SchedulerOwner::WorkItem {
                            work_item_id: wait.owner_work_item_id.clone(),
                        },
                        state: WaitState::Active,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                )]),
            },
        );
        if command.reserve_dispatch {
            next.dispatch = AgentDispatchState::Awaiting {
                wait: WaitIdentity {
                    id: wait.wait_id.clone(),
                    generation: wait.generation,
                },
            };
            next.dispatch_revision += 1;
        }
    }
    // Terminalize the replaced old focus demand if a proof was provided and validated.
    let mut transitions = vec![format!(
        "work:{}:legacy_state_adopted:generation:{}",
        command.work_item_id, command.demand.scheduling_generation
    )];
    if let Some(proof) = &command.replace_completed_focus {
        if let Some(old_demand) = next.work.get_mut(&proof.work_item_id) {
            old_demand.status = WorkStatus::Terminal;
            transitions.push(format!("work:{}:legacy_focus_replaced", proof.work_item_id));
        }
    }
    if command.focus {
        next.focus = Some(command.work_item_id.clone());
    }
    ProtocolCommandOutcome {
        outcome: Outcome {
            decision: Decision::LegacyWorkStateAdopted,
            transitions,
            diagnostics: Vec::new(),
            snapshot: next,
        },
        conflict: None,
    }
}

fn authority_matches_issue(
    authority: &ActivationAdmissionAuthority,
    command: &IssueActivationAuthorityCommand,
) -> bool {
    authority.authority_id == command.authority_id
        && authority.activation == command.activation
        && authority.expected_scheduling_generation == command.expected_scheduling_generation
        && authority.expected_dispatch_revision == command.expected_dispatch_revision
}

fn issue_activation_authority(
    snapshot: &Snapshot,
    command: &IssueActivationAuthorityCommand,
) -> ProtocolCommandOutcome {
    if snapshot.activation_authorities.values().any(|authority| {
        authority.activation.id == command.activation.id
            || authority.activation.idempotency_key == command.activation.idempotency_key
    }) {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::IdentityConflict,
                "activation_authority_identity_conflict",
            ),
        );
    }

    let authority = ActivationAdmissionAuthority {
        authority_id: command.authority_id.clone(),
        activation: command.activation.clone(),
        expected_scheduling_generation: command.expected_scheduling_generation,
        expected_dispatch_revision: command.expected_dispatch_revision,
        consumed_by: None,
    };
    let mut validating = snapshot.clone();
    validating
        .activation_authorities
        .insert(command.authority_id.clone(), authority.clone());
    let admission = AdmitActivationCommand {
        authority_id: command.authority_id.clone(),
        activation: command.activation.clone(),
        expected_scheduling_generation: command.expected_scheduling_generation,
        expected_dispatch_revision: command.expected_dispatch_revision,
    };
    if let Err(conflict) = lower_admit_activation(&validating, &admission, false) {
        return rejected_command(snapshot, conflict);
    }

    ProtocolCommandOutcome {
        outcome: Outcome {
            decision: Decision::AuthorityIssued,
            transitions: vec![format!(
                "activation_authority:{}:issued:{}",
                command.authority_id, command.activation.id
            )],
            diagnostics: Vec::new(),
            snapshot: validating,
        },
        conflict: None,
    }
}

fn lower_admit_activation(
    snapshot: &Snapshot,
    command: &AdmitActivationCommand,
    allow_consumed_authority: bool,
) -> Result<Event, ProtocolConflict> {
    let activation = &command.activation;
    if command.authority_id.is_empty()
        || activation.id.is_empty()
        || activation.agent_id.is_empty()
        || activation.idempotency_key.is_empty()
        || activation.provenance.source_id.is_empty()
        || activation
            .provenance
            .correlation_id
            .as_ref()
            .is_some_and(String::is_empty)
        || activation
            .provenance
            .causation_id
            .as_ref()
            .is_some_and(String::is_empty)
        || !activation_cause_has_identity(&activation.cause)
        || !activation_binding_has_identity(&activation.binding)
    {
        return Err(command_conflict(
            ProtocolConflictKind::InvalidCommand,
            "activation_identity_or_provenance_required",
        ));
    }
    let authority = snapshot
        .activation_authorities
        .get(&command.authority_id)
        .ok_or_else(|| {
            command_conflict(
                ProtocolConflictKind::NotFound,
                "activation_authority_not_found",
            )
        })?;
    if authority.authority_id != command.authority_id
        || authority.activation != command.activation
        || authority.expected_scheduling_generation != command.expected_scheduling_generation
        || authority.expected_dispatch_revision != command.expected_dispatch_revision
    {
        return Err(command_conflict(
            ProtocolConflictKind::BindingConflict,
            "activation_authority_mismatch",
        ));
    }
    if authority.consumed_by.is_some()
        && !(allow_consumed_authority
            && authority.consumed_by.as_deref() == Some(activation.id.as_str()))
    {
        return Err(command_conflict(
            ProtocolConflictKind::Duplicate,
            "activation_authority_already_consumed",
        ));
    }
    if !activation_provenance_matches_cause(&activation.provenance, &activation.cause) {
        return Err(command_conflict(
            ProtocolConflictKind::InvalidCommand,
            "activation_provenance_authority_mismatch",
        ));
    }
    if activation.state != ActivationLifecycleState::Admitted {
        return Err(command_conflict(
            ProtocolConflictKind::StateConflict,
            "activation_must_enter_as_admitted",
        ));
    }

    let (owner, cause) = match (&activation.cause, &activation.binding) {
        (
            ActivationCause::WorkItemRunnable {
                work_item_id,
                scheduling_generation,
            },
            ActivationBinding::WorkItem {
                work_item_id: bound_work_item_id,
            },
        ) if work_item_id == bound_work_item_id
            && *scheduling_generation == command.expected_scheduling_generation =>
        {
            (
                SchedulerOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                },
                AdmissionCause::Scheduling,
            )
        }
        (
            ActivationCause::TaskRejoin {
                task_id,
                message_id,
                resume,
            },
            ActivationBinding::WorkItem { work_item_id },
        ) => (
            SchedulerOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            AdmissionCause::TaskRejoin {
                task_id: task_id.clone(),
                message_id: message_id.clone(),
                resume: resume.clone(),
            },
        ),
        (
            ActivationCause::OperatorInput { message_id, resume },
            ActivationBinding::WorkItem { work_item_id },
        ) => (
            SchedulerOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            AdmissionCause::OperatorInput {
                message_id: message_id.clone(),
                resume: resume.clone(),
            },
        ),
        (
            ActivationCause::WaitResume {
                wait_id,
                wait_generation,
                trigger_id,
                trigger_generation,
            },
            ActivationBinding::WaitOwner {
                wait_id: bound_wait_id,
                owner,
            },
        ) if wait_id == bound_wait_id => (
            owner.clone(),
            AdmissionCause::WaitResume {
                wait_id: wait_id.clone(),
                wait_generation: *wait_generation,
                trigger_id: trigger_id.clone(),
                trigger_generation: *trigger_generation,
            },
        ),
        (
            ActivationCause::WaitResume {
                wait_id,
                wait_generation,
                trigger_id,
                trigger_generation,
            },
            ActivationBinding::Lifecycle { agent_id },
        ) if agent_id == &activation.agent_id => (
            SchedulerOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            },
            AdmissionCause::WaitResume {
                wait_id: wait_id.clone(),
                wait_generation: *wait_generation,
                trigger_id: trigger_id.clone(),
                trigger_generation: *trigger_generation,
            },
        ),
        (
            ActivationCause::LifecycleExternalNudge { message_id },
            ActivationBinding::Lifecycle { agent_id },
        ) if agent_id == &activation.agent_id => (
            SchedulerOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            },
            AdmissionCause::LifecycleExternalNudge {
                message_id: message_id.clone(),
            },
        ),
        (
            ActivationCause::SettlementRecovery { activation_id },
            ActivationBinding::WorkItem { work_item_id },
        ) => (
            SchedulerOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            AdmissionCause::SettlementRecovery {
                missing_activation_id: activation_id.clone(),
            },
        ),
        (
            ActivationCause::WorkItemRunnable { .. }
            | ActivationCause::TaskRejoin { .. }
            | ActivationCause::OperatorInput { .. }
            | ActivationCause::WaitResume { .. }
            | ActivationCause::LifecycleExternalNudge { .. }
            | ActivationCause::SettlementRecovery { .. },
            _,
        ) => {
            return Err(command_conflict(
                ProtocolConflictKind::BindingConflict,
                "activation_cause_binding_mismatch",
            ));
        }
        _ => {
            return Err(command_conflict(
                ProtocolConflictKind::UnsupportedCommand,
                "activation_cause_not_supported_by_kernel",
            ));
        }
    };

    Ok(Event::Admit {
        activation_id: activation.id.clone(),
        owner,
        expected_generation: command.expected_scheduling_generation,
        expected_dispatch_revision: command.expected_dispatch_revision,
        cause,
    })
}

fn attach_activation_input(snapshot: &Snapshot, attachment: &ActivationInputAttachment) -> Outcome {
    let ActivationSlot::Running {
        activation_id,
        owner,
        admitted_generation,
        ..
    } = &snapshot.slot
    else {
        return rejected(snapshot, "activation_input_requires_running_slot");
    };
    if activation_id != &attachment.activation_id {
        return rejected(snapshot, "activation_input_owner_mismatch");
    }
    if owner != &attachment.owner || admitted_generation != &attachment.expected_admitted_generation
    {
        return rejected(snapshot, "activation_input_owner_binding_mismatch");
    }
    if snapshot.dispatch_revision != attachment.expected_dispatch_revision {
        return rejected(snapshot, "stale_dispatch_revision");
    }
    let Some(activation) = snapshot.activations.get(activation_id) else {
        return rejected(snapshot, "running_activation_record_missing");
    };
    if activation.state != ActivationState::Running {
        return rejected(snapshot, "activation_input_owner_not_running");
    }
    let Some(admission) = snapshot.activation_admissions.get(activation_id) else {
        return rejected(snapshot, "activation_has_no_canonical_admission");
    };
    if admission.activation.preemption != PreemptionPolicy::AllowOperatorInterjection {
        return rejected(snapshot, "activation_disallows_operator_interjection");
    }
    let mut next = snapshot.clone();
    next.activation_inputs
        .insert(attachment.id.clone(), attachment.clone());
    Outcome {
        decision: Decision::ActivationInputAttached,
        transitions: vec![format!(
            "activation:{}:input_attached:{}:{}",
            attachment.activation_id, attachment.message_id, attachment.boundary
        )],
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn lower_activation_settlement(
    snapshot: &Snapshot,
    command: &SettleActivationCommand,
) -> Result<Event, ProtocolConflict> {
    let settlement = &command.settlement;
    if settlement.id.is_empty()
        || settlement.activation_id.is_empty()
        || settlement.created_at.is_empty()
        || settlement
            .turn_terminal
            .as_ref()
            .is_some_and(String::is_empty)
        || settlement
            .operator_delivery
            .as_ref()
            .is_some_and(String::is_empty)
        || settlement.evidence.iter().any(String::is_empty)
    {
        return Err(command_conflict(
            ProtocolConflictKind::InvalidCommand,
            "settlement_identity_and_created_at_required",
        ));
    }
    if matches!(
        settlement.disposition,
        ActivationDisposition::WorkCompleted { .. }
    ) && (settlement.turn_terminal.is_none()
        || settlement.operator_delivery.is_none()
        || settlement.evidence.is_empty())
    {
        return Err(command_conflict(
            ProtocolConflictKind::InvalidCommand,
            "completion_report_binding_required",
        ));
    }
    if let ActivationDisposition::WorkCompleted {
        continuation: Some(continuation),
    } = &settlement.disposition
    {
        let activation = snapshot
            .activations
            .get(&settlement.activation_id)
            .ok_or_else(|| {
                command_conflict(
                    ProtocolConflictKind::NotFound,
                    "settlement_activation_missing",
                )
            })?;
        if activation.owner.work_item_id() == Some(continuation.caller_work_item_id.as_str()) {
            return Err(command_conflict(
                ProtocolConflictKind::BindingConflict,
                "continuation_caller_is_completed_work_item",
            ));
        }
    }

    let lowered = match (&settlement.disposition, &settlement.agent_dispatch) {
        (ActivationDisposition::WorkContinues, AgentDispatchDisposition::Open) => {
            Settlement::Continue
        }
        (
            ActivationDisposition::WorkWaits { wait },
            AgentDispatchDisposition::Awaiting {
                wait: dispatch_wait,
            },
        ) if !wait.id.is_empty() && wait.generation > 0 && wait == dispatch_wait => {
            Settlement::Wait {
                wait: wait.clone(),
                mode: WaitMode::AwaitThis,
                legacy_wait_id: false,
            }
        }
        (ActivationDisposition::WorkWaits { wait }, AgentDispatchDisposition::Open)
            if !wait.id.is_empty() && wait.generation > 0 =>
        {
            Settlement::Wait {
                wait: wait.clone(),
                mode: WaitMode::AcceptScheduling,
                legacy_wait_id: false,
            }
        }
        (ActivationDisposition::WorkCompleted { continuation }, AgentDispatchDisposition::Open)
            if continuation.as_ref().is_none_or(|continuation| {
                !continuation.admission_id.is_empty()
                    && !continuation.caller_work_item_id.is_empty()
            }) =>
        {
            Settlement::Complete {
                continuation: continuation.clone(),
            }
        }
        (
            ActivationDisposition::WorkWaits { wait },
            AgentDispatchDisposition::Open | AgentDispatchDisposition::Awaiting { .. },
        ) if wait.id.is_empty() => {
            return Err(command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "wait_identity_required",
            ));
        }
        (
            ActivationDisposition::WorkWaits { wait },
            AgentDispatchDisposition::Open | AgentDispatchDisposition::Awaiting { .. },
        ) if wait.generation == 0 => {
            return Err(command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "wait_generation_required",
            ));
        }
        (
            ActivationDisposition::WorkCompleted {
                continuation: Some(_),
            },
            AgentDispatchDisposition::Open,
        ) => {
            return Err(command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "continuation_identity_required",
            ));
        }
        (
            ActivationDisposition::WorkYielded {
                target_work_item_id: None,
                continuation_id: None,
                expected_target_generation: None,
            },
            AgentDispatchDisposition::Open,
        ) => Settlement::Yield,
        (
            ActivationDisposition::WorkYielded {
                target_work_item_id: Some(target_work_item_id),
                continuation_id: Some(continuation_id),
                expected_target_generation: Some(target_generation),
            },
            AgentDispatchDisposition::Open,
        ) => Settlement::TargetedYield {
            continuation: YieldContinuationRecord {
                continuation_id: continuation_id.clone(),
                source_work_item_id: snapshot.activations[&settlement.activation_id]
                    .owner
                    .work_item_id()
                    .expect("targeted yield requires a WorkItem owner")
                    .to_string(),
                source_generation: snapshot.activations[&settlement.activation_id]
                    .admitted_generation,
                target_work_item_id: target_work_item_id.clone(),
                target_generation: *target_generation,
            },
        },
        (ActivationDisposition::WorkYielded { .. }, AgentDispatchDisposition::Open) => {
            return Err(command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "yield_continuation_identity_required",
            ));
        }
        (ActivationDisposition::WorkWaits { .. }, AgentDispatchDisposition::Awaiting { .. })
        | (
            ActivationDisposition::WorkContinues
            | ActivationDisposition::WorkCompleted { .. }
            | ActivationDisposition::WorkYielded { .. },
            AgentDispatchDisposition::Awaiting { .. },
        ) => {
            return Err(command_conflict(
                ProtocolConflictKind::BindingConflict,
                "settlement_dispatch_disposition_mismatch",
            ));
        }
        _ => {
            return Err(command_conflict(
                ProtocolConflictKind::UnsupportedCommand,
                "settlement_disposition_not_supported_by_kernel",
            ));
        }
    };

    Ok(Event::Settle {
        activation_id: settlement.activation_id.clone(),
        settlement: lowered,
    })
}

fn validate_continuation_target(
    snapshot: &Snapshot,
    completed_work_item_id: &str,
    continuation: &Continuation,
) -> Result<(), &'static str> {
    if continuation.caller_work_item_id == completed_work_item_id {
        return Err("continuation_caller_is_completed_work_item");
    }
    let Some(caller) = snapshot.work.get(&continuation.caller_work_item_id) else {
        return Err("continuation_caller_missing");
    };
    if caller.status != WorkStatus::Runnable {
        return Err("continuation_caller_not_runnable");
    }
    if caller.scheduling_generation != continuation.expected_caller_generation {
        return Err("stale_continuation_caller_generation");
    }
    Ok(())
}

fn activation_work_item_id(activation: &AgentActivation) -> Option<&str> {
    match &activation.binding {
        ActivationBinding::WorkItem { work_item_id } => Some(work_item_id),
        ActivationBinding::WaitOwner {
            owner: SchedulerOwner::WorkItem { work_item_id },
            ..
        } => Some(work_item_id),
        ActivationBinding::WaitOwner {
            owner: SchedulerOwner::AgentLifecycle { .. },
            ..
        }
        | ActivationBinding::Interaction { .. }
        | ActivationBinding::Lifecycle { .. }
        | ActivationBinding::Unbound => None,
    }
}

fn reducer_conflict(code: &str) -> ProtocolConflict {
    let kind = match code {
        "typed_protocol_command_required" => ProtocolConflictKind::UnsupportedCommand,
        "stale_dispatch_revision"
        | "stale_rollout_config_revision"
        | "stale_rollout_manifest_revision"
        | "stale_rollout_preflight_revision" => ProtocolConflictKind::StaleRevision,
        "stale_scheduling_generation"
        | "stale_wait_generation"
        | "stale_recovery_generation"
        | "stale_activation_generation"
        | "stale_continuation_caller_generation"
        | "wait_settlement_generation_mismatch"
        | "wait_generation_not_advanced" => ProtocolConflictKind::StaleGeneration,
        "activation_already_running"
        | "activation_already_settled"
        | "scheduling_generation_already_admitted"
        | "settlement_recovery_already_attempted"
        | "continuation_already_admitted" => ProtocolConflictKind::Duplicate,
        "unknown_work_item"
        | "unknown_wait"
        | "unknown_missing_settlement"
        | "running_work_item_missing"
        | "running_activation_record_missing"
        | "continuation_caller_missing" => ProtocolConflictKind::NotFound,
        "activation_id_mismatch"
        | "wait_owner_mismatch"
        | "missing_settlement_owner_mismatch"
        | "agent_lane_reserved"
        | "agent_lane_reserved_for_other_wait"
        | "wait_id_owner_mismatch"
        | "wait_id_consumed_by_other_activation"
        | "running_activation_record_mismatch"
        | "continuation_caller_is_completed_work_item" => ProtocolConflictKind::BindingConflict,
        "wait_trigger_identity_mismatch" | "conflicting_wait_trigger" => {
            ProtocolConflictKind::PayloadConflict
        }
        _ => ProtocolConflictKind::StateConflict,
    };
    command_conflict(kind, code)
}

fn activation_cause_has_identity(cause: &ActivationCause) -> bool {
    match cause {
        ActivationCause::OperatorInput { message_id, .. }
        | ActivationCause::OperatorInterjection { message_id }
        | ActivationCause::MessageIngress { message_id }
        | ActivationCause::InternalFollowup { message_id } => !message_id.is_empty(),
        ActivationCause::TaskRejoin {
            task_id,
            message_id,
            ..
        } => !task_id.is_empty() && !message_id.is_empty(),
        ActivationCause::WaitResume {
            wait_id,
            trigger_id,
            ..
        } => !wait_id.is_empty() && !trigger_id.is_empty(),
        ActivationCause::LifecycleExternalNudge { message_id } => !message_id.is_empty(),
        ActivationCause::WorkItemRunnable { work_item_id, .. }
        | ActivationCause::WorkItemRecheck { work_item_id, .. } => !work_item_id.is_empty(),
        ActivationCause::RuntimeRecovery { recovery_id } => !recovery_id.is_empty(),
        ActivationCause::SettlementRecovery { activation_id } => !activation_id.is_empty(),
    }
}

fn activation_binding_has_identity(binding: &ActivationBinding) -> bool {
    match binding {
        ActivationBinding::Unbound => true,
        ActivationBinding::WorkItem { work_item_id } => !work_item_id.is_empty(),
        ActivationBinding::WaitOwner { wait_id, owner } => {
            !wait_id.is_empty()
                && match owner {
                    SchedulerOwner::WorkItem { work_item_id } => !work_item_id.is_empty(),
                    SchedulerOwner::AgentLifecycle { agent_id } => !agent_id.is_empty(),
                }
        }
        ActivationBinding::Interaction { interaction_id } => !interaction_id.is_empty(),
        ActivationBinding::Lifecycle { agent_id } => !agent_id.is_empty(),
    }
}

fn activation_provenance_matches_cause(
    provenance: &ActivationProvenance,
    cause: &ActivationCause,
) -> bool {
    match cause {
        ActivationCause::OperatorInput { .. } | ActivationCause::OperatorInterjection { .. } => {
            provenance.origin == ActivationOrigin::Operator
                && provenance.trust == ActivationTrust::OperatorInstruction
        }
        ActivationCause::MessageIngress { .. } => {
            matches!(
                provenance.origin,
                ActivationOrigin::Channel | ActivationOrigin::Webhook | ActivationOrigin::Callback
            ) && matches!(
                provenance.trust,
                ActivationTrust::IntegrationSignal | ActivationTrust::ExternalEvidence
            )
        }
        ActivationCause::TaskRejoin { .. } => {
            provenance.origin == ActivationOrigin::Task
                && provenance.trust == ActivationTrust::RuntimeInstruction
        }
        ActivationCause::WaitResume { .. } => {
            matches!(
                provenance.origin,
                ActivationOrigin::Channel
                    | ActivationOrigin::Webhook
                    | ActivationOrigin::Callback
                    | ActivationOrigin::Timer
                    | ActivationOrigin::System
                    | ActivationOrigin::Operator
            ) && activation_provenance_has_valid_authority(provenance)
        }
        ActivationCause::LifecycleExternalNudge { .. } => {
            matches!(
                provenance.origin,
                ActivationOrigin::Channel
                    | ActivationOrigin::Webhook
                    | ActivationOrigin::Callback
                    | ActivationOrigin::Timer
                    | ActivationOrigin::System
                    | ActivationOrigin::Operator
            ) && activation_provenance_has_valid_authority(provenance)
        }
        ActivationCause::WorkItemRunnable { .. }
        | ActivationCause::WorkItemRecheck { .. }
        | ActivationCause::InternalFollowup { .. } => {
            provenance.origin == ActivationOrigin::System
                && provenance.trust == ActivationTrust::RuntimeInstruction
        }
        ActivationCause::RuntimeRecovery { .. } | ActivationCause::SettlementRecovery { .. } => {
            matches!(
                provenance.origin,
                ActivationOrigin::System | ActivationOrigin::RuntimeRecovery
            ) && provenance.trust == ActivationTrust::RuntimeInstruction
        }
    }
}

fn command_conflict(kind: ProtocolConflictKind, code: &str) -> ProtocolConflict {
    ProtocolConflict {
        kind,
        code: code.to_string(),
    }
}

fn admission_fence(
    owner: &SchedulerOwner,
    expected_generation: u64,
    cause: &AdmissionCause,
) -> String {
    let owner_identity = match owner {
        SchedulerOwner::WorkItem { work_item_id } => format!("work:{work_item_id}"),
        SchedulerOwner::AgentLifecycle { agent_id } => format!("lifecycle:{agent_id}"),
    };
    match cause {
        AdmissionCause::SettlementRecovery {
            missing_activation_id,
        } => format!("{owner_identity}:{expected_generation}:recovery:{missing_activation_id}"),
        AdmissionCause::TaskRejoin { task_id, .. } => format!("task:{task_id}"),
        AdmissionCause::OperatorInput { message_id, .. } => {
            format!("operator_message:{message_id}")
        }
        AdmissionCause::LifecycleExternalNudge { message_id } => {
            format!("lifecycle_message:{message_id}")
        }
        AdmissionCause::Scheduling | AdmissionCause::WaitResume { .. } => {
            format!("{owner_identity}:{expected_generation}")
        }
    }
}

fn consume_wait_resume_claim(
    snapshot: &Snapshot,
    next: &mut Snapshot,
    activation_id: &str,
    owner: &SchedulerOwner,
    claim: &WaitResumeClaim,
    transitions: &mut Vec<String>,
) -> Result<(), &'static str> {
    let Some(wait) = snapshot.waits.get(&claim.wait_id) else {
        return Err("unknown_wait");
    };
    if wait.current_generation != claim.wait_generation {
        return Err("stale_wait_generation");
    }
    let generation = wait
        .generations
        .get(&claim.wait_generation)
        .expect("current wait generation exists");
    if &generation.owner != owner {
        return Err("wait_owner_mismatch");
    }
    if generation.state != WaitState::Triggered {
        return Err("wait_not_triggered");
    }
    if generation.trigger
        != Some(WaitTrigger {
            trigger_id: claim.trigger_id.clone(),
            trigger_generation: claim.trigger_generation,
        })
    {
        return Err("wait_trigger_identity_mismatch");
    }
    if let SchedulerOwner::WorkItem { work_item_id } = owner {
        let Some(work) = snapshot.work.get(work_item_id) else {
            return Err("unknown_work_item");
        };
        if work.status
            != (WorkStatus::Waiting {
                wait_id: claim.wait_id.clone(),
            })
        {
            return Err("work_item_not_waiting_for_wait");
        }
    }
    if let AgentDispatchState::Awaiting {
        wait: reserved_wait,
    } = &snapshot.dispatch
    {
        if reserved_wait.id != claim.wait_id || reserved_wait.generation != claim.wait_generation {
            return Err("agent_lane_reserved_for_other_wait");
        }
    }
    let consumed = next
        .waits
        .get_mut(&claim.wait_id)
        .expect("wait exists")
        .generations
        .get_mut(&claim.wait_generation)
        .expect("current wait generation exists");
    consumed.state = WaitState::Consumed;
    consumed.consuming_activation_id = Some(activation_id.to_string());
    transitions.push(format!(
        "wait:{}:generation:{}:triggered->consumed:{}",
        claim.wait_id, claim.wait_generation, activation_id
    ));
    if matches!(
        snapshot.dispatch,
        AgentDispatchState::Awaiting { wait: ref reserved_wait }
            if reserved_wait.id == claim.wait_id
                && reserved_wait.generation == claim.wait_generation
    ) {
        set_dispatch_state(next, AgentDispatchState::Open);
    }
    Ok(())
}

fn admit(
    snapshot: &Snapshot,
    activation_id: &str,
    owner: &SchedulerOwner,
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

    let work = match owner {
        SchedulerOwner::WorkItem { work_item_id } => {
            let Some(work) = snapshot.work.get(work_item_id) else {
                return rejected(snapshot, "unknown_work_item");
            };
            if work.scheduling_generation != expected_generation {
                return rejected(snapshot, "stale_scheduling_generation");
            }
            Some(work)
        }
        SchedulerOwner::AgentLifecycle { agent_id } if agent_id.is_empty() => {
            return rejected(snapshot, "lifecycle_owner_identity_required");
        }
        SchedulerOwner::AgentLifecycle { .. } => None,
    };
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
            let Some(work) = work else {
                return rejected(snapshot, "scheduling_requires_work_item_owner");
            };
            if !matches!(work.status, WorkStatus::Runnable) {
                return rejected(snapshot, "work_item_not_runnable");
            }
            if !matches!(snapshot.dispatch, AgentDispatchState::Open) {
                return rejected(snapshot, "agent_lane_reserved");
            }
        }
        AdmissionCause::TaskRejoin {
            task_id,
            message_id,
            resume,
        } => {
            let Some(work) = work else {
                return rejected(snapshot, "task_rejoin_requires_work_item_owner");
            };
            if task_id.is_empty() || message_id.is_empty() {
                return rejected(snapshot, "task_rejoin_identity_required");
            }
            if let Some(claim) = resume {
                if let Err(code) = consume_wait_resume_claim(
                    snapshot,
                    &mut next,
                    activation_id,
                    owner,
                    claim,
                    &mut transitions,
                ) {
                    return rejected(snapshot, code);
                }
            } else {
                if work.status != WorkStatus::Runnable {
                    return rejected(snapshot, "work_item_not_runnable");
                }
                if !matches!(snapshot.dispatch, AgentDispatchState::Open) {
                    return rejected(snapshot, "agent_lane_reserved");
                }
            }
        }
        AdmissionCause::OperatorInput { message_id, resume } => {
            let Some(work) = work else {
                return rejected(snapshot, "operator_input_requires_work_item_owner");
            };
            if message_id.is_empty() {
                return rejected(snapshot, "operator_input_identity_required");
            }
            if let Some(claim) = resume {
                if let Err(code) = consume_wait_resume_claim(
                    snapshot,
                    &mut next,
                    activation_id,
                    owner,
                    claim,
                    &mut transitions,
                ) {
                    return rejected(snapshot, code);
                }
            } else {
                if work.status != WorkStatus::Runnable {
                    return rejected(snapshot, "work_item_not_runnable");
                }
                if !matches!(snapshot.dispatch, AgentDispatchState::Open) {
                    return rejected(snapshot, "agent_lane_reserved");
                }
            }
        }
        AdmissionCause::WaitResume {
            wait_id,
            wait_generation,
            trigger_id,
            trigger_generation,
        } => {
            let claim = WaitResumeClaim {
                wait_id: wait_id.clone(),
                wait_generation: *wait_generation,
                trigger_id: trigger_id.clone(),
                trigger_generation: *trigger_generation,
            };
            if let Err(code) = consume_wait_resume_claim(
                snapshot,
                &mut next,
                activation_id,
                owner,
                &claim,
                &mut transitions,
            ) {
                return rejected(snapshot, code);
            }
        }
        AdmissionCause::LifecycleExternalNudge { message_id } => {
            if message_id.is_empty() {
                return rejected(snapshot, "lifecycle_external_nudge_identity_required");
            }
            if !matches!(owner, SchedulerOwner::AgentLifecycle { .. }) {
                return rejected(
                    snapshot,
                    "lifecycle_external_nudge_requires_lifecycle_owner",
                );
            }
            if let AgentDispatchState::Awaiting { wait } = &snapshot.dispatch {
                let Some(generation) = snapshot
                    .waits
                    .get(&wait.id)
                    .and_then(|record| record.generations.get(&wait.generation))
                else {
                    return rejected(snapshot, "agent_lane_reservation_missing");
                };
                if generation.owner != *owner
                    || !matches!(generation.state, WaitState::Active | WaitState::Triggered)
                {
                    return rejected(snapshot, "agent_lane_reserved_by_other_owner");
                }
            }
        }
        AdmissionCause::SettlementRecovery {
            missing_activation_id,
        } => {
            let Some(work) = work else {
                return rejected(snapshot, "settlement_recovery_requires_work_item_owner");
            };
            let work_item_id = owner
                .work_item_id()
                .expect("settlement recovery owner is a WorkItem");
            if !matches!(snapshot.dispatch, AgentDispatchState::Open) {
                return rejected(snapshot, "settlement_recovery_lane_reserved");
            }
            let Some(missing) = snapshot.activations.get(missing_activation_id) else {
                return rejected(snapshot, "unknown_missing_settlement");
            };
            if missing.owner != *owner {
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
            let first_pending = snapshot
                .work
                .iter()
                .filter_map(|(candidate_work_item_id, demand)| match &demand.status {
                    WorkStatus::NeedsSettlement { activation_id } => {
                        Some((candidate_work_item_id.as_str(), activation_id.as_str()))
                    }
                    _ => None,
                })
                .min();
            if first_pending != Some((work_item_id, missing_activation_id.as_str())) {
                return rejected(snapshot, "settlement_recovery_not_first_pending");
            }
            recovery_for = Some(missing_activation_id.clone());
            transitions.push(format!(
                "settlement:{missing_activation_id}:awaiting_recovery->running:{activation_id}"
            ));
        }
    }

    let admission_fence = admission_fence(owner, expected_generation, cause);
    if snapshot.admitted_generations.contains(&admission_fence) {
        return rejected(snapshot, "scheduling_generation_already_admitted");
    }
    next.admitted_generations.insert(admission_fence);
    next.activations.insert(
        activation_id.to_string(),
        ActivationRecord {
            owner: owner.clone(),
            admitted_generation: expected_generation,
            state: ActivationState::Running,
            recovery_for: recovery_for.clone(),
        },
    );
    next.slot = ActivationSlot::Running {
        activation_id: activation_id.to_string(),
        owner: owner.clone(),
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
        owner,
        admitted_generation,
        recovery_for,
    } = &snapshot.slot
    else {
        return rejected(snapshot, "no_running_activation");
    };
    if running_activation_id != activation_id {
        return rejected(snapshot, "activation_id_mismatch");
    }
    let Some(running_activation) = snapshot.activations.get(activation_id) else {
        return rejected(snapshot, "running_activation_record_missing");
    };
    if running_activation.owner != *owner
        || running_activation.admitted_generation != *admitted_generation
        || running_activation.state != ActivationState::Running
        || running_activation.recovery_for != *recovery_for
    {
        return rejected(snapshot, "running_activation_record_mismatch");
    }
    match owner {
        SchedulerOwner::WorkItem { work_item_id } => settle_work_item(
            snapshot,
            activation_id,
            work_item_id,
            *admitted_generation,
            recovery_for.as_deref(),
            settlement,
        ),
        SchedulerOwner::AgentLifecycle { agent_id } => settle_lifecycle(
            snapshot,
            activation_id,
            agent_id,
            *admitted_generation,
            settlement,
        ),
    }
}

fn settle_work_item(
    snapshot: &Snapshot,
    activation_id: &str,
    work_item_id: &str,
    admitted_generation: u64,
    recovery_for: Option<&str>,
    settlement: &Settlement,
) -> Outcome {
    let running_activation_id = activation_id;
    let Some(current_work) = snapshot.work.get(work_item_id) else {
        return rejected(snapshot, "running_work_item_missing");
    };
    if current_work.scheduling_generation != admitted_generation {
        return rejected(snapshot, "stale_activation_generation");
    }
    if let Settlement::Complete {
        continuation: Some(continuation),
    } = settlement
    {
        if continuation.admission_id.is_empty() || continuation.caller_work_item_id.is_empty() {
            return rejected(snapshot, "continuation_identity_required");
        }
        if let Err(code) = validate_continuation_target(snapshot, work_item_id, continuation) {
            return rejected(snapshot, code);
        }
    }
    if let Settlement::TargetedYield { continuation } = settlement {
        if continuation.continuation_id.is_empty()
            || continuation.source_work_item_id != work_item_id
            || continuation.source_generation != admitted_generation
        {
            return rejected(snapshot, "yield_continuation_identity_mismatch");
        }
        if continuation.target_work_item_id == work_item_id {
            return rejected(snapshot, "yield_target_is_source_work_item");
        }
        let Some(target) = snapshot.work.get(&continuation.target_work_item_id) else {
            return rejected(snapshot, "yield_target_missing");
        };
        if target.scheduling_generation != continuation.target_generation {
            return rejected(snapshot, "stale_yield_target_generation");
        }
        if target.status != WorkStatus::Runnable {
            return rejected(snapshot, "yield_target_not_runnable");
        }
        if snapshot.settlements.values().any(|settlement| {
            matches!(
                &settlement.disposition,
                ActivationDisposition::WorkYielded {
                    continuation_id: Some(existing_id),
                    ..
                } if existing_id == &continuation.continuation_id
            )
        }) || snapshot.work.values().any(|demand| {
            matches!(
                &demand.status,
                WorkStatus::Yielded {
                    continuation: existing,
                } if existing.continuation_id == continuation.continuation_id
            )
        }) {
            return rejected(snapshot, "yield_continuation_already_used");
        }
        if snapshot.work.values().any(|demand| {
            matches!(
                &demand.status,
                WorkStatus::Yielded {
                    continuation: existing,
                } if existing.target_work_item_id == continuation.target_work_item_id
                    && existing.target_generation == continuation.target_generation
            )
        }) {
            return rejected(snapshot, "yield_target_generation_already_reserved");
        }
    }
    if matches!(settlement, Settlement::Complete { .. }) {
        let matching_yields = snapshot
            .work
            .values()
            .filter(|demand| {
                matches!(
                    &demand.status,
                    WorkStatus::Yielded { continuation }
                        if continuation.target_work_item_id == work_item_id
                            && continuation.target_generation == admitted_generation
                )
            })
            .count();
        if matching_yields > 1 {
            return rejected(snapshot, "ambiguous_yield_continuation");
        }
    }
    let settlement_owner_activation = recovery_for.unwrap_or(running_activation_id);
    let owner = SchedulerOwner::WorkItem {
        work_item_id: work_item_id.to_string(),
    };
    let consumed_wait_id = snapshot.waits.iter().find_map(|(wait_id, wait)| {
        wait.generations
            .get(&wait.current_generation)
            .is_some_and(|generation| {
                generation.owner == owner
                    && generation.state == WaitState::Consumed
                    && generation.consuming_activation_id.as_deref()
                        == Some(settlement_owner_activation)
            })
            .then(|| wait_id.clone())
    });
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
        Settlement::TargetedYield { continuation } => {
            resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            next.work
                .get_mut(work_item_id)
                .expect("running work item exists")
                .status = WorkStatus::Yielded {
                continuation: continuation.clone(),
            };
            set_dispatch_state(&mut next, AgentDispatchState::Open);
            next.focus = Some(continuation.target_work_item_id.clone());
            transitions.push(format!(
                "work:{work_item_id}:yielded:{}:{}",
                continuation.continuation_id, continuation.target_work_item_id
            ));
        }
        Settlement::Wait {
            wait,
            mode,
            legacy_wait_id,
        } => {
            let wait_generation = if *legacy_wait_id {
                next_generation
            } else {
                wait.generation
            };
            if wait_generation != next_generation {
                return rejected(snapshot, "wait_settlement_generation_mismatch");
            }
            let wait = WaitIdentity {
                id: wait.id.clone(),
                generation: wait_generation,
            };
            if let Some(existing_wait) = next.waits.get(&wait.id) {
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
                if current_generation.owner != owner {
                    return rejected(snapshot, "wait_id_owner_mismatch");
                }
                if existing_wait.current_generation >= next_generation {
                    return rejected(snapshot, "wait_generation_not_advanced");
                }
                if existing_wait.generations.contains_key(&next_generation) {
                    return rejected(snapshot, "wait_generation_already_exists");
                }
                if existing_wait
                    .generations
                    .keys()
                    .any(|generation| *generation > existing_wait.current_generation)
                {
                    return rejected(snapshot, "wait_history_has_future_generation");
                }
                if current_generation.state == WaitState::Consumed
                    && (consumed_wait_id.as_deref() != Some(wait.id.as_str())
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
                wait_id: wait.id.clone(),
            };
            if consumed_wait_id.as_deref().is_some_and(|id| id != wait.id) {
                resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            }
            let previous_generation = next
                .waits
                .get(&wait.id)
                .map(|record| record.current_generation);
            if let Some(record) = next.waits.get_mut(&wait.id) {
                let previous = record
                    .generations
                    .get_mut(&record.current_generation)
                    .expect("current wait generation exists");
                if previous.state == WaitState::Consumed {
                    previous.state = WaitState::Resolved;
                    previous.consuming_activation_id = None;
                    transitions.push(format!(
                        "wait:{}:generation:{}:consumed->resolved",
                        wait.id, record.current_generation
                    ));
                }
                record.current_generation = next_generation;
                record.generations.insert(
                    next_generation,
                    WaitGenerationRecord {
                        owner: owner.clone(),
                        state: WaitState::Active,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                );
            } else {
                next.waits.insert(
                    wait.id.clone(),
                    WaitRecord {
                        current_generation: next_generation,
                        generations: BTreeMap::from([(
                            next_generation,
                            WaitGenerationRecord {
                                owner: owner.clone(),
                                state: WaitState::Active,
                                trigger: None,
                                consuming_activation_id: None,
                            },
                        )]),
                    },
                );
            }
            let dispatch = match mode {
                WaitMode::AwaitThis => AgentDispatchState::Awaiting { wait: wait.clone() },
                WaitMode::AcceptScheduling => AgentDispatchState::Open,
            };
            set_dispatch_state(&mut next, dispatch);
            transitions.push(match previous_generation {
                Some(generation) => {
                    format!(
                        "wait:{}:generation:{generation}->{next_generation}:active",
                        wait.id
                    )
                }
                None => format!(
                    "wait:{}:generation:{next_generation}:created:active",
                    wait.id
                ),
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
            let restored_focus = continuation
                .as_ref()
                .map(|continuation| continuation.caller_work_item_id.clone());
            if let Some(continuation) = continuation {
                if next
                    .continuation_admissions
                    .contains_key(&continuation.admission_id)
                {
                    return rejected(snapshot, "continuation_already_admitted");
                }
                let Some(caller) = next.work.get_mut(&continuation.caller_work_item_id) else {
                    return rejected(snapshot, "continuation_caller_missing");
                };
                caller.scheduling_generation = continuation.expected_caller_generation + 1;
                caller.status = WorkStatus::Runnable;
                transitions.push(format!(
                    "continuation:{}:{}:runnable",
                    continuation.admission_id, continuation.caller_work_item_id
                ));
            }
            if next.focus.as_deref() == Some(work_item_id) {
                next.focus = restored_focus;
                transitions.push(format!("focus:{work_item_id}:released"));
            }
            let yielded_source =
                next.work
                    .iter()
                    .find_map(|(source_id, demand)| match &demand.status {
                        WorkStatus::Yielded { continuation }
                            if continuation.target_work_item_id == work_item_id
                                && continuation.target_generation == admitted_generation =>
                        {
                            Some((source_id.clone(), continuation.continuation_id.clone()))
                        }
                        _ => None,
                    });
            if let Some((source_id, continuation_id)) = yielded_source {
                next.work
                    .get_mut(&source_id)
                    .expect("yield source exists")
                    .status = WorkStatus::Runnable;
                next.focus = Some(source_id.clone());
                transitions.push(format!(
                    "yield_continuation:{continuation_id}:{source_id}:runnable"
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

fn settle_lifecycle(
    snapshot: &Snapshot,
    activation_id: &str,
    agent_id: &str,
    admitted_generation: u64,
    settlement: &Settlement,
) -> Outcome {
    if matches!(
        settlement,
        Settlement::TargetedYield { .. }
            | Settlement::Complete {
                continuation: Some(_)
            }
    ) {
        return rejected(snapshot, "lifecycle_settlement_cannot_transfer_work");
    }
    let owner = SchedulerOwner::AgentLifecycle {
        agent_id: agent_id.to_string(),
    };
    let lifecycle_nudge = snapshot
        .activation_admissions
        .get(activation_id)
        .is_some_and(|admission| {
            matches!(
                admission.activation.cause,
                ActivationCause::LifecycleExternalNudge { .. }
            )
        });
    let consumed_wait_id = snapshot.waits.iter().find_map(|(wait_id, wait)| {
        wait.generations
            .get(&wait.current_generation)
            .is_some_and(|generation| {
                generation.owner == owner
                    && generation.state == WaitState::Consumed
                    && generation.consuming_activation_id.as_deref() == Some(activation_id)
            })
            .then(|| wait_id.clone())
    });
    if matches!(settlement, Settlement::Missing) {
        let mut next = snapshot.clone();
        next.slot = ActivationSlot::Idle;
        next.activations
            .get_mut(activation_id)
            .expect("running activation exists")
            .state = ActivationState::SettlementMissing;
        return Outcome {
            decision: Decision::SettlementMissing,
            transitions: vec![
                format!("activation:{activation_id}:running->settlement_missing"),
                format!("slot:running:{activation_id}->idle"),
            ],
            diagnostics: vec![format!("settlement_missing:{activation_id}")],
            snapshot: next,
        };
    }

    let mut next = snapshot.clone();
    let mut transitions = vec![format!("activation:{activation_id}:settled")];
    match settlement {
        Settlement::Continue | Settlement::Yield | Settlement::Complete { continuation: None } => {
            if !lifecycle_nudge {
                resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
                set_dispatch_state(&mut next, AgentDispatchState::Open);
            }
        }
        Settlement::Wait {
            wait,
            mode,
            legacy_wait_id,
        } => {
            let next_generation = admitted_generation
                .checked_add(1)
                .expect("lifecycle wait generation overflow");
            let wait_generation = if *legacy_wait_id {
                next_generation
            } else {
                wait.generation
            };
            if wait_generation != next_generation {
                return rejected(snapshot, "wait_settlement_generation_mismatch");
            }
            let wait = WaitIdentity {
                id: wait.id.clone(),
                generation: wait_generation,
            };
            if let Some(existing_wait) = next.waits.get(&wait.id) {
                let current = existing_wait
                    .generations
                    .get(&existing_wait.current_generation)
                    .expect("current wait generation exists");
                if matches!(current.state, WaitState::Active | WaitState::Triggered) {
                    return rejected(snapshot, "wait_id_still_active");
                }
                if current.owner != owner {
                    return rejected(snapshot, "wait_id_owner_mismatch");
                }
                if existing_wait.current_generation >= next_generation {
                    return rejected(snapshot, "wait_generation_not_advanced");
                }
            }
            if consumed_wait_id.as_deref().is_some_and(|id| id != wait.id) {
                resolve_consumed_wait(&mut next, consumed_wait_id.as_deref(), &mut transitions);
            }
            if lifecycle_nudge {
                for (existing_wait_id, record) in &mut next.waits {
                    if existing_wait_id == &wait.id {
                        continue;
                    }
                    let generation = record
                        .generations
                        .get_mut(&record.current_generation)
                        .expect("current wait generation exists");
                    if generation.owner == owner
                        && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                    {
                        generation.state = WaitState::Resolved;
                        generation.trigger = None;
                        generation.consuming_activation_id = None;
                        transitions.push(format!(
                            "wait:{existing_wait_id}:generation:{}:resolved_by_lifecycle_rearm",
                            record.current_generation
                        ));
                    }
                }
            }
            let previous_generation = next
                .waits
                .get(&wait.id)
                .map(|record| record.current_generation);
            if let Some(record) = next.waits.get_mut(&wait.id) {
                let previous = record
                    .generations
                    .get_mut(&record.current_generation)
                    .expect("current wait generation exists");
                if previous.state == WaitState::Consumed {
                    previous.state = WaitState::Resolved;
                    previous.consuming_activation_id = None;
                }
                record.current_generation = next_generation;
                record.generations.insert(
                    next_generation,
                    WaitGenerationRecord {
                        owner: owner.clone(),
                        state: WaitState::Active,
                        trigger: None,
                        consuming_activation_id: None,
                    },
                );
            } else {
                next.waits.insert(
                    wait.id.clone(),
                    WaitRecord {
                        current_generation: next_generation,
                        generations: BTreeMap::from([(
                            next_generation,
                            WaitGenerationRecord {
                                owner: owner.clone(),
                                state: WaitState::Active,
                                trigger: None,
                                consuming_activation_id: None,
                            },
                        )]),
                    },
                );
            }
            set_dispatch_state(
                &mut next,
                match mode {
                    WaitMode::AwaitThis => AgentDispatchState::Awaiting { wait: wait.clone() },
                    WaitMode::AcceptScheduling => AgentDispatchState::Open,
                },
            );
            transitions.push(match previous_generation {
                Some(generation) => {
                    format!(
                        "wait:{}:generation:{generation}->{next_generation}:active",
                        wait.id
                    )
                }
                None => format!(
                    "wait:{}:generation:{next_generation}:created:active",
                    wait.id
                ),
            });
        }
        Settlement::TargetedYield { .. }
        | Settlement::Complete {
            continuation: Some(_),
        }
        | Settlement::Missing => unreachable!("lifecycle settlement shape handled above"),
    }
    next.slot = ActivationSlot::Idle;
    next.activations
        .get_mut(activation_id)
        .expect("running activation exists")
        .state = ActivationState::Settled;
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
    generation.consuming_activation_id = None;
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
    if !rollout_manifest_is_installable(manifest) {
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
    if !rollout_manifest_is_installable(manifest) {
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
    explicit_mode: bool,
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
    let current_mode = snapshot
        .rollout
        .scenarios
        .get(scenario_class)
        .map(|scenario| scenario.mode)
        .unwrap_or(ScenarioMode::Off);
    if mode == ScenarioMode::Authoritative && current_mode != ScenarioMode::Shadow {
        return rejected(snapshot, "scenario_not_shadow");
    }
    if !matches!(
        (current_mode, mode),
        (ScenarioMode::Off, ScenarioMode::Shadow)
            | (ScenarioMode::Shadow, ScenarioMode::Off)
            | (ScenarioMode::Shadow, ScenarioMode::Authoritative)
            | (ScenarioMode::Authoritative, ScenarioMode::Shadow)
    ) {
        return rejected(snapshot, "invalid_scenario_authority_transition");
    }
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
        if !manifest.preflight_succeeded {
            return rejected(snapshot, "rollout_preflight_failed");
        }
        let Some(class) = class else {
            return rejected(snapshot, "scenario_not_approved_for_authority");
        };
        if explicit_mode {
            if !SchedulerScenarioClass::PRODUCTION_AUTHORITY
                .iter()
                .any(|candidate| candidate.as_str() == scenario_class)
            {
                return rejected(snapshot, "scenario_not_in_production_authority_scope");
            }
        } else {
            if class.configured_mode != ScenarioMode::Authoritative {
                return rejected(snapshot, "scenario_not_approved_for_authority");
            }
            if !rollout_class_evidence_is_complete(scenario_class, class) {
                return rejected(snapshot, "rollout_class_evidence_incomplete");
            }
        }
        if snapshot.rollout.hard_blockers.iter().any(|blocker| {
            blocker.scenario_class == scenario_class
                && blocker.manifest_revision == manifest.revision
                && blocker.preflight_revision == manifest.preflight_revision
        }) {
            return rejected(snapshot, "scenario_has_unresolved_hard_blocker");
        }
        let Some(parsed_scenario) = scenario_class.parse::<SchedulerScenarioClass>().ok() else {
            return rejected(snapshot, "unknown_scenario_class");
        };
        if parsed_scenario
            .authoritative_dependencies()
            .iter()
            .any(|dependency| {
                snapshot
                    .rollout
                    .scenarios
                    .get(dependency.as_str())
                    .is_none_or(|authority| authority.mode != ScenarioMode::Authoritative)
            })
        {
            return rejected(snapshot, "scenario_authority_dependency_not_satisfied");
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
    let mut transitions = vec![format!("rollout:scenario:{scenario_class}:{mode:?}")];
    if mode != ScenarioMode::Authoritative {
        cascade_rollout_dependents(
            &mut next,
            scenario_class,
            &mut transitions,
            "dependency_authority_lowered",
        );
    }
    next.rollout.config_revision += 1;
    Outcome {
        decision: Decision::ScenarioAuthorityChanged,
        transitions,
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
    let mut transitions = vec![
        format!("rollout:hard_blocker:{scenario_class}:{blocker_code}"),
        format!(
            "rollout:scenario:{scenario_class}:authoritative->{rollback_target:?}:stop_admissions_and_revert"
        ),
    ];
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
    cascade_rollout_dependents(
        &mut next,
        scenario_class,
        &mut transitions,
        "dependency_hard_blocker",
    );
    next.rollout.config_revision += 1;
    Outcome {
        decision: Decision::RollbackTripped,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

fn cascade_rollout_dependents(
    snapshot: &mut Snapshot,
    changed_scenario: &str,
    transitions: &mut Vec<String>,
    reason: &str,
) {
    let dependent = SchedulerScenarioClass::OperatorInterjection;
    if !dependent
        .authoritative_dependencies()
        .iter()
        .any(|dependency| dependency.as_str() == changed_scenario)
    {
        return;
    }
    let Some(authority) = snapshot.rollout.scenarios.get_mut(dependent.as_str()) else {
        return;
    };
    if authority.mode != ScenarioMode::Authoritative {
        return;
    }
    let target = authority.rollback_target;
    authority.mode = target;
    authority.manifest_revision = None;
    authority.preflight_revision = None;
    transitions.push(format!(
        "rollout:scenario:{}:authoritative->{target:?}:{reason}",
        dependent.as_str()
    ));
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
    let scenario_class = scenario_class.parse::<SchedulerScenarioClass>().ok()?;
    let gate = match scenario_class {
        SchedulerScenarioClass::ReducerOnlyCandidates => RolloutClassGate {
            minimum_shadow_samples: 10_000,
            minimum_shadow_duration_secs: 72 * 60 * 60,
            required_evidence: &["deterministic_replay", "duplicate_command_idempotency"],
        },
        SchedulerScenarioClass::ExactTaskRejoin => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_task_result",
                "out_of_order_task_result",
                "restart_before_rejoin_settlement",
            ],
        },
        SchedulerScenarioClass::ExactWaitResume => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_trigger",
                "stale_generation",
                "restart_after_consume",
                "rearm",
            ],
        },
        SchedulerScenarioClass::ExplicitlyBoundOperatorInput
        | SchedulerScenarioClass::OperatorInterjection => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_ingress",
                "stale_binding_revision",
                "wrong_agent_target",
            ],
        },
        SchedulerScenarioClass::Settlement => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_settlement",
                "missing_settlement_recovery",
                "restart_before_settlement_commit",
            ],
        },
        SchedulerScenarioClass::Delivery => RolloutClassGate {
            minimum_shadow_samples: 1_000,
            minimum_shadow_duration_secs: 7 * 24 * 60 * 60,
            required_evidence: &[
                "duplicate_delivery",
                "delivery_retry",
                "restart_before_delivery_commit",
            ],
        },
        SchedulerScenarioClass::WorkItemAutonomousContinuation => RolloutClassGate {
            minimum_shadow_samples: 2_000,
            minimum_shadow_duration_secs: 14 * 24 * 60 * 60,
            required_evidence: &[
                "concurrent_claim",
                "reservation_conflict",
                "yield_return",
                "work_item_rollback",
            ],
        },
        SchedulerScenarioClass::OrdinarySemanticOperatorBinding => RolloutClassGate {
            minimum_shadow_samples: 5_000,
            minimum_shadow_duration_secs: 14 * 24 * 60 * 60,
            required_evidence: &[
                "ambiguous_input",
                "low_confidence_input",
                "conflicting_proposals",
                "zero_wrong_automatic_bindings",
            ],
        },
    };
    Some(gate)
}

fn rollout_class_evidence_is_installable(
    scenario_class: &str,
    class: &RolloutClassEvidence,
) -> bool {
    let Some(gate) = rollout_class_gate(scenario_class) else {
        return false;
    };
    let mandatory_evidence: BTreeSet<&str> = UNIVERSAL_ROLLOUT_EVIDENCE
        .iter()
        .chain(gate.required_evidence.iter())
        .copied()
        .collect();

    matches!(
        class.configured_mode,
        ScenarioMode::Shadow | ScenarioMode::Authoritative
    ) && class.minimum_shadow_samples >= gate.minimum_shadow_samples
        && class.minimum_shadow_duration_secs >= gate.minimum_shadow_duration_secs
        && class.maximum_p99_latency_regression_bps <= MAXIMUM_P99_LATENCY_REGRESSION_BPS
        && class.observed_p99_latency_regression_bps <= class.maximum_p99_latency_regression_bps
        && mandatory_evidence
            .iter()
            .all(|evidence| class.required_evidence.contains(*evidence))
        && class
            .verified_evidence
            .iter()
            .all(|evidence| class.required_evidence.contains(evidence))
        && class.rollback_policy.trigger == RollbackTrigger::AnyHardBlocker
        && rollback_target(&class.rollback_policy) != ScenarioMode::Authoritative
        && (class.configured_mode == ScenarioMode::Shadow
            || rollout_class_evidence_is_complete(scenario_class, class))
}

pub(crate) fn rollout_class_evidence_is_complete(
    scenario_class: &str,
    class: &RolloutClassEvidence,
) -> bool {
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

fn rollout_manifest_is_installable(manifest: &RolloutManifest) -> bool {
    manifest.preflight_for_manifest_revision == manifest.revision
        && manifest.preflight_succeeded
        && !manifest.protocol_build.is_empty()
        && !manifest.schema_build.is_empty()
        && manifest.schema_revision > 0
        && !manifest.fixture_corpus_revision.is_empty()
        && !manifest.classes.is_empty()
        && manifest.classes.iter().all(|(scenario_class, class)| {
            rollout_class_evidence_is_installable(scenario_class, class)
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

pub(crate) fn managed_shadow_rollout_manifest(
    revision: u64,
    preflight_revision: u64,
    protocol_build: String,
    schema_build: String,
    schema_revision: u64,
) -> RolloutManifest {
    let classes = SchedulerScenarioClass::PRODUCTION_AUTHORITY
        .into_iter()
        .map(|scenario_class| {
            let gate = rollout_class_gate(scenario_class.as_str())
                .expect("production scheduler scenario has a rollout gate");
            let required_evidence = UNIVERSAL_ROLLOUT_EVIDENCE
                .iter()
                .chain(gate.required_evidence.iter())
                .map(|evidence| (*evidence).to_string())
                .collect();
            (
                scenario_class.as_str().to_string(),
                RolloutClassEvidence {
                    configured_mode: ScenarioMode::Shadow,
                    minimum_shadow_samples: gate.minimum_shadow_samples,
                    minimum_shadow_duration_secs: gate.minimum_shadow_duration_secs,
                    observed_shadow_samples: 0,
                    observed_shadow_duration_secs: 0,
                    maximum_p99_latency_regression_bps: MAXIMUM_P99_LATENCY_REGRESSION_BPS,
                    observed_p99_latency_regression_bps: 0,
                    hard_blocker_count: 0,
                    unresolved_divergence_count: 0,
                    required_evidence,
                    verified_evidence: BTreeSet::new(),
                    rollback_policy: RollbackPolicy {
                        trigger: RollbackTrigger::AnyHardBlocker,
                        action: RollbackAction::StopAdmissionsAndRevert {
                            target: ScenarioMode::Shadow,
                        },
                    },
                },
            )
        })
        .collect();
    RolloutManifest {
        revision,
        preflight_revision,
        preflight_for_manifest_revision: revision,
        preflight_succeeded: true,
        protocol_build,
        schema_build,
        schema_revision,
        fixture_corpus_revision: "runtime-managed-shadow-v1".into(),
        classes,
        safety_divergence_bps: 0,
        canonical_state_divergence_bps: 0,
        allowed_observational_divergence: BTreeMap::new(),
        approver: "operator:HOLON_SCHEDULER".into(),
        approved_at: "runtime-managed-shadow".into(),
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
    for (scenario_class, authority) in &snapshot.rollout.scenarios {
        if authority.mode != ScenarioMode::Authoritative {
            continue;
        }
        let scenario = scenario_class
            .parse::<SchedulerScenarioClass>()
            .map_err(|_| "rollout contains an unknown scenario class".to_string())?;
        if scenario
            .authoritative_dependencies()
            .iter()
            .any(|dependency| {
                snapshot
                    .rollout
                    .scenarios
                    .get(dependency.as_str())
                    .is_none_or(|authority| authority.mode != ScenarioMode::Authoritative)
            })
        {
            return Err(format!(
                "authoritative rollout scenario {scenario_class} has non-authoritative dependencies"
            ));
        }
    }
    if let Some(focus) = &snapshot.focus {
        let work = snapshot
            .work
            .get(focus)
            .ok_or_else(|| "focus references unknown work item".to_string())?;
        if work.status == WorkStatus::Terminal {
            return Err("terminal work item retains focus".into());
        }
    }
    let mut idempotency_keys = BTreeSet::new();
    let mut canonical_admission_fences = BTreeSet::new();
    let mut recovery_targets = BTreeSet::new();
    for (activation_id, command) in &snapshot.activation_admissions {
        let Some(activation) = snapshot.activations.get(activation_id) else {
            return Err("canonical activation admission record is invalid".into());
        };
        let event = lower_admit_activation(snapshot, command, true)
            .map_err(|_| "canonical activation admission record is invalid".to_string())?;
        let Event::Admit {
            activation_id: event_activation_id,
            owner,
            expected_generation,
            expected_dispatch_revision,
            cause,
        } = event
        else {
            unreachable!("activation admission lowers to admission event");
        };
        let recovery_for = match &cause {
            AdmissionCause::SettlementRecovery {
                missing_activation_id,
            } => {
                if !recovery_targets.insert(missing_activation_id.clone()) {
                    return Err(
                        "canonical activation admissions reuse a settlement recovery fence".into(),
                    );
                }
                Some(missing_activation_id.clone())
            }
            AdmissionCause::Scheduling
            | AdmissionCause::TaskRejoin { .. }
            | AdmissionCause::OperatorInput { .. }
            | AdmissionCause::WaitResume { .. }
            | AdmissionCause::LifecycleExternalNudge { .. } => None,
        };
        if !canonical_admission_fences.insert(admission_fence(&owner, expected_generation, &cause))
        {
            return Err("canonical activation admissions reuse an admission fence".into());
        }
        if activation_id != &command.activation.id
            || event_activation_id != *activation_id
            || activation.owner != owner
            || activation.admitted_generation != expected_generation
            || activation.recovery_for != recovery_for
            || expected_dispatch_revision > snapshot.dispatch_revision
            || snapshot
                .activation_authorities
                .get(&command.authority_id)
                .and_then(|authority| authority.consumed_by.as_deref())
                != Some(activation_id.as_str())
            || !idempotency_keys.insert(command.activation.idempotency_key.as_str())
        {
            return Err(
                "canonical activation admission record disagrees with authority state".into(),
            );
        }
    }
    if snapshot.admitted_generations != canonical_admission_fences {
        return Err(format!(
            "canonical admission fences disagree with activation admissions: persisted={:?}, canonical={canonical_admission_fences:?}",
            snapshot.admitted_generations
        ));
    }
    let mut authority_activation_ids = BTreeSet::new();
    let mut authority_idempotency_keys = BTreeSet::new();
    for (authority_id, authority) in &snapshot.activation_authorities {
        if authority_id != &authority.authority_id {
            return Err("activation authority map key disagrees with authority identity".into());
        }
        if !authority_activation_ids.insert(authority.activation.id.as_str())
            || !authority_idempotency_keys.insert(authority.activation.idempotency_key.as_str())
        {
            return Err("activation authorities reuse activation identity".into());
        }
        if let Some(activation_id) = &authority.consumed_by {
            let Some(command) = snapshot.activation_admissions.get(activation_id) else {
                return Err("consumed activation authority has no canonical admission".into());
            };
            if authority_id != &command.authority_id
                || authority.activation != command.activation
                || authority.expected_scheduling_generation
                    != command.expected_scheduling_generation
                || authority.expected_dispatch_revision != command.expected_dispatch_revision
            {
                return Err("consumed activation authority disagrees with admission".into());
            }
        }
    }
    let mut activation_input_message_ids = BTreeSet::new();
    for (attachment_id, attachment) in &snapshot.activation_inputs {
        let Some(activation) = snapshot.activations.get(&attachment.activation_id) else {
            return Err("activation input references unknown activation".into());
        };
        if attachment_id != &attachment.id
            || activation.owner != attachment.owner
            || activation.admitted_generation != attachment.expected_admitted_generation
            || attachment.expected_dispatch_revision > snapshot.dispatch_revision
            || !activation_input_message_ids.insert(attachment.message_id.as_str())
            || attachment.provenance.source_id != attachment.message_id
            || attachment.provenance.origin != ActivationOrigin::Operator
            || attachment.provenance.trust != ActivationTrust::OperatorInstruction
        {
            return Err("canonical activation input attachment is invalid".into());
        }
        let Some(admission) = snapshot
            .activation_admissions
            .get(&attachment.activation_id)
        else {
            return Err("activation input owner has no canonical admission".into());
        };
        if admission.activation.preemption != PreemptionPolicy::AllowOperatorInterjection {
            return Err("activation input owner disallows operator interjection".into());
        }
    }
    let mut settled_activations = BTreeSet::new();
    let mut terminal_records = BTreeMap::<String, usize>::new();
    let mut current_awaiting_settlements = BTreeSet::<(String, u64)>::new();
    let mut expected_continuation_admissions =
        BTreeMap::<String, ContinuationAdmissionRecord>::new();
    let mut continuation_prestate_fences = BTreeSet::<(String, u64)>::new();
    for (settlement_id, settlement) in &snapshot.settlements {
        let Some(activation) = snapshot.activations.get(&settlement.activation_id) else {
            return Err("canonical settlement references unknown activation".into());
        };
        let event = lower_activation_settlement(
            snapshot,
            &SettleActivationCommand {
                settlement: settlement.clone(),
            },
        )
        .map_err(|_| "canonical activation settlement record is invalid".to_string())?;
        let Event::Settle {
            settlement: lowered,
            ..
        } = event
        else {
            unreachable!("activation settlement lowers to settlement event");
        };
        if settlement_id != &settlement.id
            || activation.state != ActivationState::Settled
            || !snapshot
                .activation_admissions
                .contains_key(&settlement.activation_id)
            || !settled_activations.insert(settlement.activation_id.as_str())
        {
            return Err("canonical activation settlement record is invalid".into());
        }
        *terminal_records
            .entry(settlement.activation_id.clone())
            .or_default() += 1;
        let settlement_generation = activation.admitted_generation + 1;
        let Some(work_item_id) = activation.owner.work_item_id() else {
            match lowered {
                Settlement::Continue
                | Settlement::Yield
                | Settlement::Complete { continuation: None } => {}
                Settlement::Wait {
                    wait,
                    mode,
                    legacy_wait_id,
                } => {
                    if legacy_wait_id || wait.generation != settlement_generation {
                        return Err(
                            "canonical lifecycle wait settlement has an invalid generation".into(),
                        );
                    }
                    let Some(generation) = snapshot
                        .waits
                        .get(&wait.id)
                        .and_then(|wait| wait.generations.get(&settlement_generation))
                    else {
                        return Err(
                            "canonical lifecycle wait settlement has no authoritative wait fact"
                                .into(),
                        );
                    };
                    if generation.owner != activation.owner {
                        return Err("canonical lifecycle wait settlement owner mismatch".into());
                    }
                    if mode == WaitMode::AwaitThis
                        && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                    {
                        current_awaiting_settlements.insert((wait.id, wait.generation));
                    }
                }
                Settlement::TargetedYield { .. }
                | Settlement::Complete {
                    continuation: Some(_),
                }
                | Settlement::Missing => {
                    return Err("canonical lifecycle settlement has an invalid disposition".into());
                }
            }
            continue;
        };
        let Some(work) = snapshot.work.get(work_item_id) else {
            return Err("canonical settlement references unknown work item".into());
        };
        if work.scheduling_generation <= activation.admitted_generation {
            return Err("canonical activation settlement has a stale work generation".into());
        }
        let has_successor_activation =
            snapshot
                .activations
                .iter()
                .any(|(candidate_id, candidate)| {
                    candidate.owner == activation.owner
                        && candidate.admitted_generation == settlement_generation
                        && snapshot.activation_admissions.contains_key(candidate_id)
                });
        let projects_current_work_state =
            work.scheduling_generation == settlement_generation && !has_successor_activation;
        match lowered {
            Settlement::Continue | Settlement::Yield => {
                if projects_current_work_state && work.status != WorkStatus::Runnable {
                    return Err(
                        "canonical runnable settlement disagrees with authoritative work state"
                            .into(),
                    );
                }
            }
            Settlement::TargetedYield { continuation } => {
                let restored = work.status == WorkStatus::Runnable
                    && snapshot.activations.values().any(|candidate| {
                        candidate.owner.work_item_id()
                            == Some(continuation.target_work_item_id.as_str())
                            && candidate.admitted_generation == continuation.target_generation
                            && candidate.state == ActivationState::Settled
                    });
                if projects_current_work_state
                    && work.status
                        != (WorkStatus::Yielded {
                            continuation: continuation.clone(),
                        })
                    && !restored
                {
                    return Err(
                        "canonical targeted yield disagrees with authoritative work state".into(),
                    );
                }
            }
            Settlement::Wait {
                wait,
                mode,
                legacy_wait_id,
            } => {
                if legacy_wait_id {
                    return Err("canonical settlement retained a legacy wait shape".into());
                }
                if wait.generation != settlement_generation {
                    return Err("canonical wait settlement has stale reservation generation".into());
                }
                let Some(generation) = snapshot
                    .waits
                    .get(&wait.id)
                    .and_then(|wait| wait.generations.get(&settlement_generation))
                else {
                    return Err(
                        "canonical wait settlement has no matching authoritative wait fact".into(),
                    );
                };
                if generation.owner != activation.owner {
                    return Err(
                        "canonical wait settlement has no matching authoritative wait fact".into(),
                    );
                }
                if projects_current_work_state
                    && work.status
                        != (WorkStatus::Waiting {
                            wait_id: wait.id.clone(),
                        })
                {
                    return Err(
                        "canonical wait settlement disagrees with authoritative work state".into(),
                    );
                }
                if projects_current_work_state
                    && mode == WaitMode::AwaitThis
                    && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                {
                    current_awaiting_settlements.insert((wait.id, wait.generation));
                }
            }
            Settlement::Complete {
                continuation: Some(continuation),
            } => {
                let admitted_caller_generation = continuation
                    .expected_caller_generation
                    .checked_add(1)
                    .ok_or_else(|| "canonical continuation generation overflow".to_string())?;
                if !continuation_prestate_fences.insert((
                    continuation.caller_work_item_id.clone(),
                    continuation.expected_caller_generation,
                )) || expected_continuation_admissions
                    .insert(
                        continuation.admission_id.clone(),
                        ContinuationAdmissionRecord {
                            admission_id: continuation.admission_id.clone(),
                            settlement_id: settlement_id.clone(),
                            completed_work_item_id: work_item_id.to_string(),
                            caller_work_item_id: continuation.caller_work_item_id.clone(),
                            expected_caller_generation: continuation.expected_caller_generation,
                            expected_caller_status: WorkStatus::Runnable,
                            admitted_caller_generation,
                        },
                    )
                    .is_some()
                {
                    return Err(
                        "canonical completion settlements reuse a continuation admission".into(),
                    );
                }
            }
            Settlement::Complete { .. } => {
                if projects_current_work_state && work.status != WorkStatus::Terminal {
                    return Err(
                        "canonical completion settlement disagrees with authoritative work state"
                            .into(),
                    );
                }
            }
            Settlement::Missing => {
                return Err("terminal settlement cannot be recorded as missing".into());
            }
        }
    }
    if snapshot.continuation_admissions != expected_continuation_admissions {
        return Err(
            "canonical continuation admissions disagree with completion settlements".into(),
        );
    }
    for record in snapshot.continuation_admissions.values() {
        let Some(caller) = snapshot.work.get(&record.caller_work_item_id) else {
            return Err("canonical continuation admission references unknown caller".into());
        };
        if record.admitted_caller_generation
            != record
                .expected_caller_generation
                .checked_add(1)
                .ok_or_else(|| "canonical continuation generation overflow".to_string())?
            || record.expected_caller_status != WorkStatus::Runnable
            || caller.scheduling_generation < record.admitted_caller_generation
            || snapshot.activation_admissions.values().any(|command| {
                activation_work_item_id(&command.activation)
                    == Some(record.caller_work_item_id.as_str())
                    && command.expected_scheduling_generation == record.expected_caller_generation
            })
        {
            return Err("canonical continuation admission has an invalid caller fence".into());
        }
        let has_successor_activation = snapshot.activation_admissions.values().any(|command| {
            activation_work_item_id(&command.activation)
                == Some(record.caller_work_item_id.as_str())
                && command.expected_scheduling_generation == record.admitted_caller_generation
        });
        let has_successor_continuation =
            snapshot.continuation_admissions.values().any(|candidate| {
                candidate.admission_id != record.admission_id
                    && candidate.caller_work_item_id == record.caller_work_item_id
                    && candidate.expected_caller_generation == record.admitted_caller_generation
            });
        if !has_successor_activation
            && !has_successor_continuation
            && (caller.scheduling_generation != record.admitted_caller_generation
                || caller.status != WorkStatus::Runnable)
        {
            return Err(
                "canonical continuation admission disagrees with authoritative caller state".into(),
            );
        }
    }
    for (record_id, record) in &snapshot.missing_settlements {
        if record_id != &record.id || record.id.is_empty() || record.created_at.is_empty() {
            return Err("canonical missing-settlement record has invalid identity".into());
        }
        let Some(activation) = snapshot.activations.get(&record.activation_id) else {
            return Err("canonical missing-settlement record references unknown activation".into());
        };
        if !snapshot
            .activation_admissions
            .contains_key(&record.activation_id)
        {
            return Err(
                "canonical missing-settlement record has no canonical activation admission".into(),
            );
        }
        *terminal_records
            .entry(record.activation_id.clone())
            .or_default() += 1;
        let work = activation
            .owner
            .work_item_id()
            .map(|work_item_id| {
                snapshot.work.get(work_item_id).ok_or_else(|| {
                    "canonical missing-settlement record references unknown work item".to_string()
                })
            })
            .transpose()?;
        match activation.state {
            ActivationState::Running => {
                return Err("running activation has a canonical missing-settlement record".into());
            }
            ActivationState::SettlementMissing => {
                if work.is_none() {
                    if activation.recovery_for.is_some() {
                        return Err(
                            "lifecycle missing settlement cannot use WorkItem recovery".into()
                        );
                    }
                    continue;
                }
                let work = work.expect("checked lifecycle settlement above");
                if let Some(missing_activation_id) = &activation.recovery_for {
                    if work.status
                        != (WorkStatus::Paused {
                            hold_id: format!("settlement-recovery:{missing_activation_id}"),
                        })
                    {
                        return Err(
                            "failed recovery missing-settlement record has no typed hold".into(),
                        );
                    }
                } else if work.status
                    != (WorkStatus::NeedsSettlement {
                        activation_id: record.activation_id.clone(),
                    })
                    && work.status
                        != (WorkStatus::Paused {
                            hold_id: format!("settlement-recovery:{}", record.activation_id),
                        })
                {
                    return Err(
                        "missing-settlement record disagrees with authoritative recovery state"
                            .into(),
                    );
                }
            }
            ActivationState::Settled => {
                if activation.recovery_for.is_some()
                    || !snapshot.activations.values().any(|candidate| {
                        candidate.recovery_for.as_deref() == Some(record.activation_id.as_str())
                            && candidate.state == ActivationState::Settled
                    })
                {
                    return Err(
                        "recovered missing-settlement record has no settled recovery activation"
                            .into(),
                    );
                }
            }
        }
    }
    for (activation_id, activation) in &snapshot.activations {
        if !snapshot.activation_admissions.contains_key(activation_id) {
            return Err("canonical activation has no canonical admission".into());
        }
        let terminal_record_count = terminal_records.get(activation_id).copied().unwrap_or(0);
        let expected_terminal_record_count = match activation.state {
            ActivationState::Running => 0,
            ActivationState::Settled | ActivationState::SettlementMissing => 1,
        };
        if terminal_record_count != expected_terminal_record_count {
            return Err(
                "canonical activation lifecycle disagrees with terminal settlement records".into(),
            );
        }
        if activation.state == ActivationState::SettlementMissing
            && !snapshot
                .missing_settlements
                .values()
                .any(|record| record.activation_id == *activation_id)
        {
            return Err(
                "settlement-missing activation has no canonical missing-settlement record".into(),
            );
        }
    }
    if current_awaiting_settlements.len() > 1 {
        return Err("multiple canonical settlements reserve the single agent lane".into());
    }
    if let Some((wait_id, generation)) = current_awaiting_settlements.iter().next() {
        if snapshot.dispatch
            != (AgentDispatchState::Awaiting {
                wait: WaitIdentity {
                    id: wait_id.clone(),
                    generation: *generation,
                },
            })
        {
            return Err(
                "canonical settlement dispatch disagrees with authoritative lane state".into(),
            );
        }
    } else if !snapshot.settlements.is_empty() && snapshot.dispatch != AgentDispatchState::Open {
        let lifecycle_wait_is_preserved = match &snapshot.dispatch {
            AgentDispatchState::Awaiting { wait } => snapshot
                .waits
                .get(&wait.id)
                .and_then(|record| record.generations.get(&wait.generation))
                .is_some_and(|generation| {
                    matches!(generation.owner, SchedulerOwner::AgentLifecycle { .. })
                        && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                }),
            AgentDispatchState::Open => false,
        };
        if !lifecycle_wait_is_preserved {
            return Err(
                "canonical settlement dispatch disagrees with authoritative lane state".into(),
            );
        }
    }
    if snapshot.rollout.protocol_mode != ProtocolMode::Legacy && snapshot.rollout.manifest.is_none()
    {
        return Err("non-legacy protocol has no rollout manifest".into());
    }
    if let Some(manifest) = &snapshot.rollout.manifest {
        if !rollout_manifest_is_installable(manifest) {
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
            owner,
            admitted_generation,
            recovery_for,
        } => {
            let activation = snapshot
                .activations
                .get(activation_id)
                .ok_or_else(|| "running slot has no canonical activation".to_string())?;
            if activation.owner != *owner
                || activation.admitted_generation != *admitted_generation
                || activation.state != ActivationState::Running
                || activation.recovery_for != *recovery_for
            {
                return Err("running slot disagrees with canonical activation".into());
            }
            match owner {
                SchedulerOwner::WorkItem { work_item_id } => {
                    let work = snapshot.work.get(work_item_id).ok_or_else(|| {
                        "running activation references unknown work item".to_string()
                    })?;
                    if work.scheduling_generation != *admitted_generation {
                        return Err(
                            "running activation generation fence does not match work item".into(),
                        );
                    }
                    match recovery_for {
                        Some(missing_activation_id) => {
                            let missing = snapshot
                                .activations
                                .get(missing_activation_id)
                                .ok_or_else(|| {
                                    "recovery activation references unknown missing settlement"
                                        .to_string()
                                })?;
                            if missing.owner != *owner
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
                                "ordinary running activation has settlement-missing work state"
                                    .into(),
                            );
                        }
                        None => {}
                    }
                }
                SchedulerOwner::AgentLifecycle { agent_id } => {
                    if agent_id.is_empty() || recovery_for.is_some() {
                        return Err("running lifecycle activation has invalid owner state".into());
                    }
                }
            }
        }
    }

    for (work_item_id, work) in &snapshot.work {
        if let WorkStatus::NeedsSettlement { activation_id } = &work.status {
            let missing = snapshot
                .activations
                .get(activation_id)
                .ok_or_else(|| "needs-settlement work item has no canonical fact".to_string())?;
            if missing.owner
                != (SchedulerOwner::WorkItem {
                    work_item_id: work_item_id.clone(),
                })
                || missing.admitted_generation != work.scheduling_generation
                || missing.state != ActivationState::SettlementMissing
                || missing.recovery_for.is_some()
            {
                return Err("needs-settlement work item has inconsistent canonical fact".into());
            }
        }
    }

    for (activation_id, activation) in &snapshot.activations {
        let work = activation
            .owner
            .work_item_id()
            .map(|work_item_id| {
                snapshot
                    .work
                    .get(work_item_id)
                    .ok_or_else(|| "activation references unknown work item".to_string())
            })
            .transpose()?;
        match activation.state {
            ActivationState::Running => {
                if snapshot.slot
                    != (ActivationSlot::Running {
                        activation_id: activation_id.clone(),
                        owner: activation.owner.clone(),
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
                if work.is_none() {
                    if activation.recovery_for.is_some() {
                        return Err(
                            "lifecycle settlement recovery requires a WorkItem owner".into()
                        );
                    }
                    continue;
                }
                let work = work.expect("checked lifecycle activation above");
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

    if let AgentDispatchState::Awaiting { wait: reservation } = &snapshot.dispatch {
        let wait = snapshot
            .waits
            .get(&reservation.id)
            .ok_or_else(|| "lane reservation references unknown wait".to_string())?;
        if wait.current_generation != reservation.generation {
            return Err("lane reservation generation is stale".into());
        }
        let generation = wait
            .generations
            .get(&reservation.generation)
            .ok_or_else(|| "lane reservation references missing wait generation".to_string())?;
        if !matches!(generation.state, WaitState::Active | WaitState::Triggered) {
            return Err("lane reservation references inactive wait".into());
        }
        if let SchedulerOwner::WorkItem { work_item_id } = &generation.owner {
            let work = snapshot
                .work
                .get(work_item_id)
                .ok_or_else(|| "reserved wait references unknown owner".to_string())?;
            if work.status
                != (WorkStatus::Waiting {
                    wait_id: reservation.id.clone(),
                })
            {
                return Err("reserved wait owner is not waiting for that wait".into());
            }
        }
    }

    for (wait_id, wait) in &snapshot.waits {
        let current = wait
            .generations
            .get(&wait.current_generation)
            .ok_or_else(|| format!("wait {wait_id} is missing its current generation"))?;
        for (generation, record) in &wait.generations {
            if *generation > wait.current_generation {
                return Err(format!(
                    "wait {wait_id} has future generation {generation} beyond current generation {}",
                    wait.current_generation
                ));
            }
            if *generation != wait.current_generation && record.state != WaitState::Resolved {
                return Err(format!(
                    "wait {wait_id} has non-resolved historical generation {generation}"
                ));
            }
        }
        if matches!(current.state, WaitState::Active | WaitState::Triggered) {
            if let SchedulerOwner::WorkItem { work_item_id } = &current.owner {
                let owner = snapshot
                    .work
                    .get(work_item_id)
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
                let consuming_activation = snapshot
                    .activations
                    .get(consuming_activation_id)
                    .ok_or_else(|| {
                        format!("consumed wait {wait_id} references unknown activation")
                    })?;
                let running_consumer = consuming_activation.state == ActivationState::Running
                    && snapshot.slot
                        == (ActivationSlot::Running {
                            activation_id: consuming_activation_id.clone(),
                            owner: current.owner.clone(),
                            admitted_generation: wait.current_generation,
                            recovery_for: None,
                        })
                    && match &current.owner {
                        SchedulerOwner::WorkItem { work_item_id } => {
                            snapshot.work.get(work_item_id).is_some_and(|owner| {
                                owner.status
                                    == (WorkStatus::Waiting {
                                        wait_id: wait_id.clone(),
                                    })
                            })
                        }
                        SchedulerOwner::AgentLifecycle { .. } => true,
                    };
                let missing_consumer = consuming_activation.state
                    == ActivationState::SettlementMissing
                    && match &current.owner {
                        SchedulerOwner::AgentLifecycle { .. } => {
                            matches!(snapshot.slot, ActivationSlot::Idle)
                        }
                        SchedulerOwner::WorkItem { work_item_id } => snapshot
                            .work
                            .get(work_item_id)
                            .is_some_and(|owner| match &owner.status {
                                WorkStatus::NeedsSettlement { activation_id }
                                    if activation_id == consuming_activation_id =>
                                {
                                    matches!(snapshot.slot, ActivationSlot::Idle)
                                        || matches!(
                                            &snapshot.slot,
                                            ActivationSlot::Running {
                                                owner,
                                                admitted_generation,
                                                recovery_for: Some(recovery_for),
                                                ..
                                            } if owner == &current.owner
                                                && *admitted_generation == wait.current_generation
                                                && recovery_for == consuming_activation_id
                                        )
                                }
                                WorkStatus::Paused { hold_id }
                                    if hold_id
                                        == &format!(
                                            "settlement-recovery:{consuming_activation_id}"
                                        ) =>
                                {
                                    matches!(snapshot.slot, ActivationSlot::Idle)
                                }
                                _ => false,
                            }),
                    };
                if current.trigger.is_none()
                    || consuming_activation.owner != current.owner
                    || consuming_activation.admitted_generation != wait.current_generation
                    || consuming_activation.recovery_for.is_some()
                    || (!running_consumer && !missing_consumer)
                {
                    return Err(format!(
                        "consumed wait {wait_id} has no matching activation lifecycle"
                    ));
                }
            }
            WaitState::Resolved => {
                if let SchedulerOwner::WorkItem { work_item_id } = &current.owner {
                    let owner = snapshot.work.get(work_item_id).ok_or_else(|| {
                        format!("resolved wait {wait_id} references unknown owner")
                    })?;
                    if wait.current_generation >= owner.scheduling_generation {
                        return Err(format!(
                            "resolved wait {wait_id} is not historical for its owner"
                        ));
                    }
                }
            }
            WaitState::Active | WaitState::Triggered => {}
        }
    }

    Ok(())
}

#[cfg(test)]
mod wire_compatibility_tests {
    use std::collections::BTreeMap;

    use super::{
        managed_shadow_rollout_manifest, reduce_rollout_command, ActivationBinding,
        ActivationInputAttachment, ActivationSlot, AgentDispatchState, Decision, ProtocolMode,
        RolloutCommand, RolloutState, ScenarioAuthority, ScenarioMode, SchedulerOwner,
        SchedulerScenarioClass, Snapshot, WaitGenerationRecord, WaitState,
    };

    #[test]
    fn legacy_wait_owner_fields_deserialize_as_work_item_owner() {
        let binding: ActivationBinding = serde_json::from_value(serde_json::json!({
            "kind": "wait_owner",
            "wait_id": "wait-a",
            "owner_work_item_id": "work-a"
        }))
        .unwrap();
        assert_eq!(
            binding,
            ActivationBinding::WaitOwner {
                wait_id: "wait-a".into(),
                owner: SchedulerOwner::WorkItem {
                    work_item_id: "work-a".into(),
                },
            }
        );

        let generation: WaitGenerationRecord = serde_json::from_value(serde_json::json!({
            "owner_work_item_id": "work-a",
            "state": "active"
        }))
        .unwrap();
        assert_eq!(
            generation,
            WaitGenerationRecord {
                owner: SchedulerOwner::WorkItem {
                    work_item_id: "work-a".into(),
                },
                state: WaitState::Active,
                trigger: None,
                consuming_activation_id: None,
            }
        );
    }

    #[test]
    fn wait_owner_wire_fields_reject_missing_or_conflicting_owner() {
        let missing = serde_json::from_value::<ActivationBinding>(serde_json::json!({
            "kind": "wait_owner",
            "wait_id": "wait-a"
        }))
        .unwrap_err();
        assert!(missing.to_string().contains("scheduler owner is missing"));

        let conflicting = serde_json::from_value::<WaitGenerationRecord>(serde_json::json!({
            "owner": {
                "kind": "agent_lifecycle",
                "agent_id": "agent-a"
            },
            "owner_work_item_id": "work-a",
            "state": "active"
        }))
        .unwrap_err();
        assert!(conflicting
            .to_string()
            .contains("scheduler owner fields conflict"));
    }

    #[test]
    fn activation_input_wire_accepts_legacy_and_lifecycle_owners() {
        let legacy: ActivationInputAttachment =
            serde_json::from_value(activation_input_json(serde_json::json!({
                "work_item_id": "work-a",
                "expected_scheduling_generation": 3
            })))
            .unwrap();
        assert_eq!(
            legacy.owner,
            SchedulerOwner::WorkItem {
                work_item_id: "work-a".into(),
            }
        );

        let lifecycle: ActivationInputAttachment =
            serde_json::from_value(activation_input_json(serde_json::json!({
                "owner": {
                    "kind": "agent_lifecycle",
                    "agent_id": "agent-a"
                },
                "expected_admitted_generation": 4
            })))
            .unwrap();
        assert_eq!(
            lifecycle.owner,
            SchedulerOwner::AgentLifecycle {
                agent_id: "agent-a".into(),
            }
        );
        assert!(serde_json::to_value(lifecycle)
            .unwrap()
            .get("work_item_id")
            .is_none());
    }

    #[test]
    fn activation_input_wire_rejects_conflicting_owner_formats() {
        let error = serde_json::from_value::<ActivationInputAttachment>(activation_input_json(
            serde_json::json!({
                "owner": {
                    "kind": "agent_lifecycle",
                    "agent_id": "agent-a"
                },
                "expected_admitted_generation": 4,
                "work_item_id": "work-a",
                "expected_scheduling_generation": 3
            }),
        ))
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("exactly one complete owner/generation format"));
    }

    #[test]
    fn operator_interjection_authority_requires_all_dependencies() {
        let mut snapshot = rollout_snapshot();
        for dependency in SchedulerScenarioClass::OperatorInterjection.authoritative_dependencies()
        {
            snapshot
                .rollout
                .scenarios
                .insert(dependency.as_str().into(), authoritative_scenario());
        }
        snapshot
            .rollout
            .scenarios
            .get_mut(SchedulerScenarioClass::ExactWaitResume.as_str())
            .expect("dependency")
            .mode = ScenarioMode::Shadow;
        snapshot.rollout.scenarios.insert(
            SchedulerScenarioClass::OperatorInterjection.as_str().into(),
            ScenarioAuthority {
                mode: ScenarioMode::Shadow,
                rollback_target: ScenarioMode::Shadow,
                manifest_revision: None,
                preflight_revision: None,
            },
        );

        let outcome = reduce_rollout_command(
            &snapshot,
            &RolloutCommand::ChangeScenarioAuthority {
                scenario_class: SchedulerScenarioClass::OperatorInterjection.as_str().into(),
                expected_config_revision: 7,
                expected_manifest_revision: 1,
                expected_preflight_revision: 1,
                mode: ScenarioMode::Authoritative,
            },
        );
        assert_eq!(outcome.outcome.decision, Decision::Rejected);
        assert_eq!(
            outcome.outcome.diagnostics,
            ["scenario_authority_dependency_not_satisfied"]
        );
    }

    #[test]
    fn lowering_dependency_cascades_operator_interjection() {
        let mut snapshot = rollout_snapshot();
        for dependency in SchedulerScenarioClass::OperatorInterjection.authoritative_dependencies()
        {
            snapshot
                .rollout
                .scenarios
                .insert(dependency.as_str().into(), authoritative_scenario());
        }
        snapshot.rollout.scenarios.insert(
            SchedulerScenarioClass::OperatorInterjection.as_str().into(),
            authoritative_scenario(),
        );

        let outcome = reduce_rollout_command(
            &snapshot,
            &RolloutCommand::ChangeScenarioAuthority {
                scenario_class: SchedulerScenarioClass::ExactWaitResume.as_str().into(),
                expected_config_revision: 7,
                expected_manifest_revision: 1,
                expected_preflight_revision: 1,
                mode: ScenarioMode::Shadow,
            },
        );
        assert_eq!(outcome.outcome.decision, Decision::ScenarioAuthorityChanged);
        assert_eq!(
            outcome.outcome.snapshot.rollout.scenarios
                [SchedulerScenarioClass::OperatorInterjection.as_str()]
            .mode,
            ScenarioMode::Shadow
        );
    }

    fn rollout_snapshot() -> Snapshot {
        let mut manifest = managed_shadow_rollout_manifest(1, 1, "test".into(), "test".into(), 38);
        for class in manifest.classes.values_mut() {
            class.configured_mode = ScenarioMode::Authoritative;
            class.observed_shadow_samples = class.minimum_shadow_samples;
            class.observed_shadow_duration_secs = class.minimum_shadow_duration_secs;
            class.verified_evidence = class.required_evidence.clone();
        }
        Snapshot {
            slot: ActivationSlot::Idle,
            dispatch: AgentDispatchState::Open,
            dispatch_revision: 0,
            focus: None,
            work: BTreeMap::new(),
            waits: BTreeMap::new(),
            activations: BTreeMap::new(),
            activation_authorities: BTreeMap::new(),
            activation_admissions: BTreeMap::new(),
            settlements: BTreeMap::new(),
            missing_settlements: BTreeMap::new(),
            rollout: RolloutState {
                protocol_mode: ProtocolMode::Authoritative,
                config_revision: 7,
                manifest: Some(manifest),
                ..RolloutState::default()
            },
            admitted_generations: Default::default(),
            continuation_admissions: BTreeMap::new(),
            activation_inputs: BTreeMap::new(),
        }
    }

    fn authoritative_scenario() -> ScenarioAuthority {
        ScenarioAuthority {
            mode: ScenarioMode::Authoritative,
            rollback_target: ScenarioMode::Shadow,
            manifest_revision: Some(1),
            preflight_revision: Some(1),
        }
    }

    fn activation_input_json(owner_fields: serde_json::Value) -> serde_json::Value {
        let mut value = serde_json::json!({
            "id": "attachment-a",
            "activation_id": "activation-a",
            "expected_dispatch_revision": 0,
            "message_id": "message-a",
            "turn_id": "turn-a",
            "boundary": "before_tool_execution",
            "round": 1,
            "provenance": {
                "origin": "operator",
                "trust": "operator_instruction",
                "source_id": "message-a"
            },
            "created_at": "2026-07-28T00:00:00Z"
        });
        value
            .as_object_mut()
            .expect("attachment payload")
            .extend(owner_fields.as_object().expect("owner fields").clone());
        value
    }
}
