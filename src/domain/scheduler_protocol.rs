//! Pure deterministic scheduler protocol kernel.
//!
//! This module is the production home of the executable Scheduler / WorkItem
//! baseline. It is intentionally storage-independent.

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
    pub activation_admissions: BTreeMap<String, AdmitActivationCommand>,
    pub settlements: BTreeMap<String, ActivationSettlement>,
    pub missing_settlements: BTreeMap<String, MissingSettlementRecord>,
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
    activation_admissions: Option<BTreeMap<String, AdmitActivationCommand>>,
    settlements: Option<BTreeMap<String, ActivationSettlement>>,
    missing_settlements: Option<BTreeMap<String, MissingSettlementRecord>>,
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
        let dispatch = wire.dispatch.into_snapshot_dispatch(&waits)?;
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
            activation_admissions,
            settlements,
            missing_settlements,
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
    Interrupted,
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
    OperatorInterjection,
    Settlement,
    Delivery,
}

impl SchedulerScenarioClass {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReducerOnlyCandidates => "reducer_only_candidates",
            Self::ExactTaskRejoin => "exact_task_rejoin",
            Self::ExactWaitResume => "exact_wait_resume",
            Self::ExplicitlyBoundOperatorInput => "explicitly_bound_operator_input",
            Self::WorkItemAutonomousContinuation => "work_item_autonomous_continuation",
            Self::OperatorInterjection => "operator_interjection",
            Self::Settlement => "settlement",
            Self::Delivery => "delivery",
        }
    }
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
pub struct SettleActivationCommand {
    pub settlement: ActivationSettlement,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoverInterruptedActivationCommand {
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
pub struct LifecycleWaitHandoffProof {
    pub wait: WaitIdentity,
    pub expected_dispatch_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdoptActivationWorkStateCommand {
    pub source_activation_id: String,
    pub source_message_id: String,
    pub source_turn_id: String,
    pub source_admitted_generation: u64,
    pub work_item_id: String,
    pub source_work_item_revision: u64,
    pub wait: LegacyWaitAdoption,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_lifecycle_wait: Option<LifecycleWaitHandoffProof>,
    #[serde(default)]
    pub focus: bool,
    #[serde(default)]
    pub reserve_dispatch: bool,
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
    AdoptActivationWorkState(AdoptActivationWorkStateCommand),
    AdmitActivation(AdmitActivationCommand),
    SettleActivation(SettleActivationCommand),
    RecoverInterruptedActivation(RecoverInterruptedActivationCommand),
    RecordMissingSettlement(MissingSettlementRecord),
    TriggerWait(TriggerWaitCommand),
    AttachActivationInput(AttachActivationInputCommand),
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
    InternalFollowup {
        message_id: String,
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
    Interrupted {
        reason: String,
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
            Interrupted {
                reason: &'a str,
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
            Self::Interrupted { reason } => Wire::Interrupted { reason },
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
            Interrupted {
                reason: String,
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
            Wire::Interrupted { reason } => Ok(Self::Interrupted { reason }),
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
    Admitted,
    Settled,
    WaitTriggered,
    MetadataUpdated,
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

    let command = match event {
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
                AdmissionCause::InternalFollowup { message_id } => (
                    ActivationCause::InternalFollowup {
                        message_id: message_id.clone(),
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
            ProtocolCommand::AdmitActivation(command)
        }
        Event::Settle {
            activation_id,
            settlement: Settlement::Missing,
        } => ProtocolCommand::RecordMissingSettlement(MissingSettlementRecord {
            id: context.record_id.clone(),
            activation_id: activation_id.clone(),
            created_at: context.recorded_at.clone(),
        }),
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
                Settlement::Interrupted { reason } => (
                    ActivationDisposition::Interrupted {
                        reason: reason.clone(),
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
            })
        }
        _ => {
            return Err(command_conflict(
                ProtocolConflictKind::UnsupportedCommand,
                "event_is_not_legacy_protocol_boundary",
            ));
        }
    };

    let mut outcome = reduce_command(snapshot, &command);
    if outcome.outcome.decision == Decision::Rejected {
        outcome.outcome.snapshot = snapshot.clone();
    }
    Ok(LegacyEventMigration { command, outcome })
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
    if let ProtocolCommand::AdoptActivationWorkState(command) = command {
        return adopt_activation_work_state(snapshot, command);
    }
    if let ProtocolCommand::RecoverInterruptedActivation(command) = command {
        return recover_interrupted_activation(snapshot, command);
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
                if existing == &command.demand && wait_matches && focus_matches && dispatch_matches
                {
                    return Some(duplicate_command(
                        snapshot,
                        "legacy_work_state_already_adopted",
                    ));
                }
                // The source WorkItem revision is authoritative.  A same-revision
                // mismatch is compatibility projection drift, not an identity
                // conflict, so let the reducer refresh the legacy row.
                return None;
            }
        }
        ProtocolCommand::AdoptActivationWorkState(command) => {
            if let Some(existing) = snapshot.work.get(&command.work_item_id) {
                let wait_matches = snapshot
                    .waits
                    .get(&command.wait.wait_id)
                    .is_some_and(|wait| {
                        wait.current_generation == command.wait.generation
                            && wait.generations.get(&command.wait.generation)
                                == Some(&WaitGenerationRecord {
                                    owner: SchedulerOwner::WorkItem {
                                        work_item_id: command.work_item_id.clone(),
                                    },
                                    state: WaitState::Active,
                                    trigger: None,
                                    consuming_activation_id: None,
                                })
                    });
                if existing.metadata_revision == command.source_work_item_revision
                    && existing.scheduling_generation == command.wait.generation
                    && existing.status
                        == (WorkStatus::Waiting {
                            wait_id: command.wait.wait_id.clone(),
                        })
                    && wait_matches
                    && (!command.focus
                        || snapshot.focus.as_deref() == Some(command.work_item_id.as_str()))
                    && (!command.reserve_dispatch
                        || snapshot.dispatch
                            == (AgentDispatchState::Awaiting {
                                wait: WaitIdentity {
                                    id: command.wait.wait_id.clone(),
                                    generation: command.wait.generation,
                                },
                            }))
                {
                    return Some(duplicate_command(
                        snapshot,
                        "activation_work_state_already_adopted",
                    ));
                }
                if existing.metadata_revision < command.source_work_item_revision {
                    return None;
                }
                if existing.metadata_revision > command.source_work_item_revision {
                    return Some(rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::StaleRevision,
                            "activation_work_state_adoption_stale_revision",
                        ),
                    ));
                }
                if existing.scheduling_generation < command.wait.generation {
                    return None;
                }
                return Some(rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::StaleGeneration,
                        "activation_work_state_adoption_stale_generation",
                    ),
                ));
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
            if snapshot
                .activation_admissions
                .values()
                .any(|existing| existing.authority_id == command.authority_id)
            {
                return Some(rejected_command(
                    snapshot,
                    command_conflict(
                        ProtocolConflictKind::AuthorityConflict,
                        "activation_authority_id_command_conflict",
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
        ProtocolCommand::RecoverInterruptedActivation(command) => {
            if let Some(existing) = snapshot.settlements.get(&command.settlement.id) {
                return Some(if existing == &command.settlement {
                    duplicate_command(snapshot, "interruption_recovery_already_applied")
                } else {
                    rejected_command(
                        snapshot,
                        command_conflict(
                            ProtocolConflictKind::IdentityConflict,
                            "interruption_recovery_id_command_conflict",
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
        ProtocolCommand::RegisterWorkDemand(_)
        | ProtocolCommand::AdoptLegacyWorkState(_)
        | ProtocolCommand::AdoptActivationWorkState(_) => {
            unreachable!("work demand mutations are reduced directly")
        }
        ProtocolCommand::AdmitActivation(command) => lower_admit_activation(command),
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
        ProtocolCommand::RecoverInterruptedActivation(_) => {
            unreachable!("interruption recovery is reduced directly")
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
    let source_dispatch_wait = snapshot
        .work
        .get(&command.work_item_id)
        .and_then(|demand| match &demand.status {
            WorkStatus::Waiting { wait_id } => Some(WaitIdentity {
                id: wait_id.clone(),
                generation: demand.scheduling_generation,
            }),
            _ => None,
        })
        .filter(|wait| snapshot.dispatch == (AgentDispatchState::Awaiting { wait: wait.clone() }));
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
    if command.reserve_dispatch
        && snapshot.dispatch != AgentDispatchState::Open
        && source_dispatch_wait.is_none()
    {
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

    let mut transitions = vec![format!(
        "work:{}:legacy_state_adopted:generation:{}",
        command.work_item_id, command.demand.scheduling_generation
    )];
    let resolutions = snapshot
        .work
        .get(&command.work_item_id)
        .and_then(|existing| match &existing.status {
            WorkStatus::Waiting { wait_id } => Some(WaitIdentity {
                id: wait_id.clone(),
                generation: existing.scheduling_generation,
            }),
            _ => None,
        })
        .filter(|existing_wait| {
            !command.wait.as_ref().is_some_and(|target| {
                target.wait_id == existing_wait.id && target.generation == existing_wait.generation
            })
        })
        .and_then(|existing_wait| {
            snapshot
                .waits
                .get(&existing_wait.id)
                .and_then(|wait| wait.generations.get(&existing_wait.generation))
                .filter(|generation| {
                    matches!(generation.state, WaitState::Active | WaitState::Triggered)
                })
                .cloned()
                .map(|expected| LaneWaitResolution {
                    wait: existing_wait,
                    expected,
                    reason: "resolved_by_legacy_adoption",
                })
        })
        .into_iter()
        .collect::<Vec<_>>();
    let target_wait_already_armed = command.wait.as_ref().is_some_and(|target| {
        snapshot.waits.get(&target.wait_id).is_some_and(|wait| {
            wait.current_generation == target.generation
                && wait
                    .generations
                    .get(&target.generation)
                    .is_some_and(|generation| {
                        generation.owner.work_item_id() == Some(target.owner_work_item_id.as_str())
                            && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                    })
        })
    });
    let arm = command
        .wait
        .as_ref()
        .filter(|_| !target_wait_already_armed)
        .map(|wait| LaneWaitArm {
            wait: WaitIdentity {
                id: wait.wait_id.clone(),
                generation: wait.generation,
            },
            owner: SchedulerOwner::WorkItem {
                work_item_id: wait.owner_work_item_id.clone(),
            },
        });
    let target_dispatch = if let Some(arm) = &arm {
        if command.reserve_dispatch || source_dispatch_wait.is_some() {
            AgentDispatchState::Awaiting {
                wait: arm.wait.clone(),
            }
        } else {
            snapshot.dispatch.clone()
        }
    } else if source_dispatch_wait.is_some() {
        AgentDispatchState::Open
    } else {
        snapshot.dispatch.clone()
    };
    let mut next = snapshot.clone();
    if let Err(code) = apply_lane_transition(
        &mut next,
        &snapshot.dispatch,
        &resolutions,
        arm.as_ref(),
        target_dispatch,
        Some(LaneWorkUpdate {
            work_item_id: command.work_item_id.clone(),
            demand: command.demand.clone(),
        }),
        &mut transitions,
    ) {
        return rejected_command(
            snapshot,
            command_conflict(ProtocolConflictKind::StateConflict, code),
        );
    }
    // Terminalize the replaced old focus demand if a proof was provided and validated.
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

fn adopt_activation_work_state(
    snapshot: &Snapshot,
    command: &AdoptActivationWorkStateCommand,
) -> ProtocolCommandOutcome {
    if command.source_activation_id.is_empty()
        || command.source_message_id.is_empty()
        || command.source_turn_id.is_empty()
        || command.source_admitted_generation == 0
        || command.work_item_id.is_empty()
        || command.source_work_item_revision == 0
        || command.wait.wait_id.is_empty()
        || command.wait.owner_work_item_id != command.work_item_id
        || command.wait.generation == 0
        || command.wait.source_updated_at.is_empty()
        || command
            .source_lifecycle_wait
            .as_ref()
            .is_some_and(|proof| proof.wait.id.is_empty() || proof.wait.generation == 0)
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "activation_work_state_adoption_fields_required",
            ),
        );
    }
    let Some(activation) = snapshot.activations.get(&command.source_activation_id) else {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::NotFound,
                "activation_work_state_source_activation_missing",
            ),
        );
    };
    let source_is_lifecycle_nudge = snapshot
        .activation_admissions
        .get(&command.source_activation_id)
        .is_some_and(|admission| {
            matches!(
                admission.activation.cause,
                ActivationCause::LifecycleExternalNudge { .. }
            )
        });
    if command.source_activation_id != format!("activation:message:{}", command.source_message_id)
        || activation.owner.lifecycle_agent_id().is_none()
        || !source_is_lifecycle_nudge
        || activation.admitted_generation != command.source_admitted_generation
        || activation.state != ActivationState::Settled
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::BindingConflict,
                "activation_work_state_source_activation_mismatch",
            ),
        );
    }
    let source_settlement = snapshot.settlements.values().find(|settlement| {
        settlement.activation_id == command.source_activation_id
            && settlement.turn_terminal.as_deref() == Some(command.source_turn_id.as_str())
    });
    if !source_settlement.is_some_and(|settlement| {
        settlement.disposition == ActivationDisposition::WorkContinues
            && settlement.agent_dispatch == AgentDispatchDisposition::Open
            && settlement.operator_delivery.is_none()
    }) {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::BindingConflict,
                "activation_work_state_source_settlement_mismatch",
            ),
        );
    }
    let existing_work = snapshot.work.get(&command.work_item_id);
    let existing_wait_id = existing_work.and_then(|work| match &work.status {
        WorkStatus::Waiting { wait_id } => Some(wait_id.as_str()),
        _ => None,
    });
    if let Some(existing_work) = existing_work {
        if existing_work.metadata_revision > command.source_work_item_revision {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StaleRevision,
                    "activation_work_state_adoption_stale_revision",
                ),
            );
        }
        if existing_work.metadata_revision == command.source_work_item_revision
            && existing_work.scheduling_generation >= command.wait.generation
        {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StaleGeneration,
                    "activation_work_state_adoption_stale_generation",
                ),
            );
        }
        if existing_wait_id.is_none() {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "activation_work_state_existing_work_not_waiting",
                ),
            );
        }
        if existing_work.scheduling_generation >= command.wait.generation {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StaleGeneration,
                    "activation_work_state_adoption_stale_generation",
                ),
            );
        }
        let existing_wait_id = existing_wait_id.expect("checked existing waiting work");
        let Some(existing_wait) = snapshot.waits.get(existing_wait_id) else {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "activation_work_state_existing_wait_conflict",
                ),
            );
        };
        let Some(current) = existing_wait
            .generations
            .get(&existing_wait.current_generation)
        else {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "activation_work_state_existing_wait_conflict",
                ),
            );
        };
        if existing_wait.current_generation != existing_work.scheduling_generation
            || current.owner
                != (SchedulerOwner::WorkItem {
                    work_item_id: command.work_item_id.clone(),
                })
            || !matches!(current.state, WaitState::Active | WaitState::Triggered)
        {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::StateConflict,
                    "activation_work_state_existing_wait_conflict",
                ),
            );
        }
    }
    if let Some(existing_wait) = snapshot.waits.get(&command.wait.wait_id) {
        let current = existing_wait
            .generations
            .get(&existing_wait.current_generation)
            .expect("current wait generation exists");
        let target_is_current_wait = existing_wait_id == Some(command.wait.wait_id.as_str());
        if !target_is_current_wait
            && (current.owner
                != (SchedulerOwner::WorkItem {
                    work_item_id: command.work_item_id.clone(),
                })
                || existing_wait.current_generation >= command.wait.generation
                || current.state != WaitState::Resolved)
        {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::IdentityConflict,
                    "activation_work_state_wait_conflict",
                ),
            );
        }
    }
    let existing_work_dispatch_matches = existing_wait_id.is_some_and(|wait_id| {
        snapshot.dispatch
            == (AgentDispatchState::Awaiting {
                wait: WaitIdentity {
                    id: wait_id.to_string(),
                    generation: existing_work
                        .expect("existing wait requires existing work")
                        .scheduling_generation,
                },
            })
    });
    let source_lifecycle_dispatch_matches =
        command.source_lifecycle_wait.as_ref().is_some_and(|proof| {
            snapshot.dispatch_revision == proof.expected_dispatch_revision
                && snapshot.dispatch
                    == (AgentDispatchState::Awaiting {
                        wait: proof.wait.clone(),
                    })
                && snapshot
                    .waits
                    .get(&proof.wait.id)
                    .and_then(|wait| wait.generations.get(&proof.wait.generation))
                    .is_some_and(|generation| {
                        generation.owner == activation.owner
                            && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                    })
        });
    if command.source_lifecycle_wait.is_some() && !source_lifecycle_dispatch_matches {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::BindingConflict,
                "activation_work_state_source_lifecycle_wait_mismatch",
            ),
        );
    }
    if snapshot.dispatch != AgentDispatchState::Open
        && !existing_work_dispatch_matches
        && !source_lifecycle_dispatch_matches
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::StateConflict,
                "activation_work_state_dispatch_not_open",
            ),
        );
    }
    if snapshot.dispatch == AgentDispatchState::Open && command.source_lifecycle_wait.is_some() {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::BindingConflict,
                "activation_work_state_source_lifecycle_wait_mismatch",
            ),
        );
    }

    let mut resolutions = Vec::new();
    if let Some(proof) = &command.source_lifecycle_wait {
        let expected = snapshot
            .waits
            .get(&proof.wait.id)
            .and_then(|wait| wait.generations.get(&proof.wait.generation))
            .expect("validated source lifecycle wait generation exists")
            .clone();
        resolutions.push(LaneWaitResolution {
            wait: proof.wait.clone(),
            expected,
            reason: "resolved_by_work_item_handoff",
        });
    }
    if let Some(existing_wait_id) = existing_wait_id {
        if command
            .source_lifecycle_wait
            .as_ref()
            .is_some_and(|proof| proof.wait.id == existing_wait_id)
        {
            return rejected_command(
                snapshot,
                command_conflict(
                    ProtocolConflictKind::IdentityConflict,
                    "activation_work_state_wait_conflict",
                ),
            );
        }
        if let Some(wait) = snapshot.waits.get(existing_wait_id) {
            let expected = wait
                .generations
                .get(&wait.current_generation)
                .expect("current wait generation exists");
            if matches!(expected.state, WaitState::Active | WaitState::Triggered) {
                resolutions.push(LaneWaitResolution {
                    wait: WaitIdentity {
                        id: existing_wait_id.to_string(),
                        generation: wait.current_generation,
                    },
                    expected: expected.clone(),
                    reason: "resolved_by_activation_rearm",
                });
            }
        }
    }
    let demand = WorkDemand {
        metadata_revision: command.source_work_item_revision,
        scheduling_generation: command.wait.generation,
        status: WorkStatus::Waiting {
            wait_id: command.wait.wait_id.clone(),
        },
        capabilities: Default::default(),
        locks: Default::default(),
        locality: "runtime".into(),
        cost_class: "default".into(),
    };
    let arm = LaneWaitArm {
        wait: WaitIdentity {
            id: command.wait.wait_id.clone(),
            generation: command.wait.generation,
        },
        owner: SchedulerOwner::WorkItem {
            work_item_id: command.work_item_id.clone(),
        },
    };
    let target_dispatch = if command.reserve_dispatch {
        AgentDispatchState::Awaiting {
            wait: arm.wait.clone(),
        }
    } else {
        AgentDispatchState::Open
    };
    let mut next = snapshot.clone();
    let mut transitions = Vec::new();
    if let Err(code) = apply_lane_transition(
        &mut next,
        &snapshot.dispatch,
        &resolutions,
        Some(&arm),
        target_dispatch,
        Some(LaneWorkUpdate {
            work_item_id: command.work_item_id.clone(),
            demand,
        }),
        &mut transitions,
    ) {
        return rejected_command(
            snapshot,
            command_conflict(ProtocolConflictKind::StateConflict, code),
        );
    }
    if command.focus {
        next.focus = Some(command.work_item_id.clone());
    }
    transitions.extend([
        format!(
            "work:{}:activation_state_adopted:generation:{}",
            command.work_item_id, command.wait.generation
        ),
        format!(
            "wait:{}:generation:{}:armed",
            command.wait.wait_id, command.wait.generation
        ),
    ]);
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

fn lower_admit_activation(command: &AdmitActivationCommand) -> Result<Event, ProtocolConflict> {
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
            ActivationCause::InternalFollowup { message_id },
            ActivationBinding::WorkItem { work_item_id },
        ) => (
            SchedulerOwner::WorkItem {
                work_item_id: work_item_id.clone(),
            },
            AdmissionCause::InternalFollowup {
                message_id: message_id.clone(),
            },
        ),
        (
            ActivationCause::InternalFollowup { message_id },
            ActivationBinding::Lifecycle { agent_id },
        ) if agent_id == &activation.agent_id => (
            SchedulerOwner::AgentLifecycle {
                agent_id: agent_id.clone(),
            },
            AdmissionCause::InternalFollowup {
                message_id: message_id.clone(),
            },
        ),
        (
            ActivationCause::WorkItemRunnable { .. }
            | ActivationCause::TaskRejoin { .. }
            | ActivationCause::OperatorInput { .. }
            | ActivationCause::WaitResume { .. }
            | ActivationCause::LifecycleExternalNudge { .. }
            | ActivationCause::SettlementRecovery { .. }
            | ActivationCause::InternalFollowup { .. }
            | ActivationCause::OperatorInterjection { .. }
            | ActivationCause::MessageIngress { .. }
            | ActivationCause::WorkItemRecheck { .. }
            | ActivationCause::RuntimeRecovery { .. },
            _,
        ) => {
            return Err(command_conflict(
                ProtocolConflictKind::BindingConflict,
                "activation_cause_binding_mismatch",
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
        (ActivationDisposition::Interrupted { reason }, AgentDispatchDisposition::Open)
            if !reason.is_empty() =>
        {
            Settlement::Interrupted {
                reason: reason.clone(),
            }
        }
        (ActivationDisposition::Interrupted { .. }, AgentDispatchDisposition::Open) => {
            return Err(command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "interruption_reason_required",
            ));
        }
        (ActivationDisposition::WorkWaits { .. }, AgentDispatchDisposition::Awaiting { .. })
        | (
            ActivationDisposition::WorkContinues
            | ActivationDisposition::WorkCompleted { .. }
            | ActivationDisposition::WorkYielded { .. }
            | ActivationDisposition::Interrupted { .. },
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
                    | ActivationOrigin::Task
            ) && activation_provenance_has_valid_authority(provenance)
        }
        ActivationCause::WorkItemRunnable { .. } | ActivationCause::WorkItemRecheck { .. } => {
            provenance.origin == ActivationOrigin::System
                && provenance.trust == ActivationTrust::RuntimeInstruction
        }
        ActivationCause::InternalFollowup { .. } => matches!(
            provenance.origin,
            ActivationOrigin::System | ActivationOrigin::Task
        ),
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
        AdmissionCause::Scheduling
        | AdmissionCause::WaitResume { .. }
        | AdmissionCause::InternalFollowup { .. } => {
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
            ActivationState::Interrupted => "activation_already_interrupted",
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
        AdmissionCause::InternalFollowup { message_id } => {
            if message_id.is_empty() {
                return rejected(snapshot, "internal_followup_identity_required");
            }
            if let Some(work) = work {
                if !matches!(work.status, WorkStatus::Runnable) {
                    return rejected(snapshot, "work_item_not_runnable");
                }
                if !matches!(snapshot.dispatch, AgentDispatchState::Open) {
                    return rejected(snapshot, "agent_lane_reserved");
                }
            } else {
                let SchedulerOwner::AgentLifecycle { .. } = owner else {
                    return rejected(snapshot, "internal_followup_owner_invalid");
                };
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

fn recover_interrupted_activation(
    snapshot: &Snapshot,
    command: &RecoverInterruptedActivationCommand,
) -> ProtocolCommandOutcome {
    let settlement = &command.settlement;
    let invalid = settlement.id.is_empty()
        || settlement.activation_id.is_empty()
        || settlement.created_at.is_empty()
        || settlement.turn_terminal.is_some()
        || settlement.operator_delivery.is_some()
        || settlement.agent_dispatch != AgentDispatchDisposition::Open
        || !matches!(
            &settlement.disposition,
            ActivationDisposition::Interrupted { reason } if !reason.is_empty()
        );
    if invalid {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::InvalidCommand,
                "interruption_recovery_shape_invalid",
            ),
        );
    }
    let Some(activation) = snapshot.activations.get(&settlement.activation_id) else {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::NotFound,
                "interruption_recovery_activation_missing",
            ),
        );
    };
    if activation.state != ActivationState::SettlementMissing || activation.recovery_for.is_some() {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::StateConflict,
                "interruption_recovery_activation_not_pending",
            ),
        );
    }
    let Some(work_item_id) = activation.owner.work_item_id() else {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::BindingConflict,
                "interruption_recovery_requires_work_item_owner",
            ),
        );
    };
    let Some(work) = snapshot.work.get(work_item_id) else {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::NotFound,
                "interruption_recovery_work_item_missing",
            ),
        );
    };
    if work.status
        != (WorkStatus::NeedsSettlement {
            activation_id: settlement.activation_id.clone(),
        })
    {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::StateConflict,
                "interruption_recovery_work_item_not_pending",
            ),
        );
    }
    let Some(missing_id) = snapshot
        .missing_settlements
        .iter()
        .find_map(|(id, record)| {
            (record.activation_id == settlement.activation_id).then(|| id.clone())
        })
    else {
        return rejected_command(
            snapshot,
            command_conflict(
                ProtocolConflictKind::NotFound,
                "interruption_recovery_missing_record_absent",
            ),
        );
    };

    let mut next = snapshot.clone();
    next.activations
        .get_mut(&settlement.activation_id)
        .expect("validated activation exists")
        .state = ActivationState::Interrupted;
    let next_work = next
        .work
        .get_mut(work_item_id)
        .expect("validated WorkItem exists");
    next_work.scheduling_generation = next_work
        .scheduling_generation
        .checked_add(1)
        .expect("WorkItem recovery generation overflow");
    next_work.status = WorkStatus::Runnable;
    next.missing_settlements.remove(&missing_id);
    next.settlements
        .insert(settlement.id.clone(), settlement.clone());

    ProtocolCommandOutcome {
        outcome: Outcome {
            decision: Decision::Settled,
            transitions: vec![
                format!(
                    "activation:{}:settlement_missing->interrupted",
                    settlement.activation_id
                ),
                format!(
                    "work:{work_item_id}:interruption_recovery:generation:{}",
                    next_work.scheduling_generation
                ),
            ],
            diagnostics: Vec::new(),
            snapshot: next,
        },
        conflict: None,
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

    let mut transitions = vec![
        format!("activation:{activation_id}:settled"),
        format!(
            "work:{work_item_id}:scheduling_generation:{}->{}",
            current_work.scheduling_generation, next_generation
        ),
    ];

    match settlement {
        Settlement::Continue | Settlement::Yield | Settlement::Interrupted { .. } => {
            let mut demand = current_work.clone();
            demand.scheduling_generation = next_generation;
            demand.status = WorkStatus::Runnable;
            let resolutions = current_lane_resolution(
                snapshot,
                consumed_wait_id.as_deref(),
                "consumed->resolved",
            )
            .into_iter()
            .collect::<Vec<_>>();
            if let Err(code) = apply_lane_transition(
                &mut next,
                &snapshot.dispatch,
                &resolutions,
                None,
                AgentDispatchState::Open,
                Some(LaneWorkUpdate {
                    work_item_id: work_item_id.to_string(),
                    demand,
                }),
                &mut transitions,
            ) {
                return rejected(snapshot, code);
            }
            transitions.push(match settlement {
                Settlement::Interrupted { reason } => {
                    format!("work:{work_item_id}:interrupted_recovery:{reason}")
                }
                _ => format!("work:{work_item_id}:runnable"),
            });
        }
        Settlement::TargetedYield { continuation } => {
            let mut demand = current_work.clone();
            demand.scheduling_generation = next_generation;
            demand.status = WorkStatus::Yielded {
                continuation: continuation.clone(),
            };
            let resolutions = current_lane_resolution(
                snapshot,
                consumed_wait_id.as_deref(),
                "consumed->resolved",
            )
            .into_iter()
            .collect::<Vec<_>>();
            if let Err(code) = apply_lane_transition(
                &mut next,
                &snapshot.dispatch,
                &resolutions,
                None,
                AgentDispatchState::Open,
                Some(LaneWorkUpdate {
                    work_item_id: work_item_id.to_string(),
                    demand,
                }),
                &mut transitions,
            ) {
                return rejected(snapshot, code);
            }
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
            if let Some(existing_wait) = snapshot.waits.get(&wait.id) {
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
            let mut demand = current_work.clone();
            demand.scheduling_generation = next_generation;
            demand.status = WorkStatus::Waiting {
                wait_id: wait.id.clone(),
            };
            let resolutions = current_lane_resolution(
                snapshot,
                consumed_wait_id.as_deref(),
                "consumed->resolved",
            )
            .into_iter()
            .collect::<Vec<_>>();
            let arm = LaneWaitArm {
                wait: wait.clone(),
                owner: owner.clone(),
            };
            let target_dispatch = match mode {
                WaitMode::AwaitThis => AgentDispatchState::Awaiting { wait: wait.clone() },
                WaitMode::AcceptScheduling => AgentDispatchState::Open,
            };
            if let Err(code) = apply_lane_transition(
                &mut next,
                &snapshot.dispatch,
                &resolutions,
                Some(&arm),
                target_dispatch,
                Some(LaneWorkUpdate {
                    work_item_id: work_item_id.to_string(),
                    demand,
                }),
                &mut transitions,
            ) {
                return rejected(snapshot, code);
            }
        }
        Settlement::Complete { continuation } => {
            let mut demand = current_work.clone();
            demand.scheduling_generation = next_generation;
            demand.status = WorkStatus::Terminal;
            let resolutions = current_lane_resolution(
                snapshot,
                consumed_wait_id.as_deref(),
                "consumed->resolved",
            )
            .into_iter()
            .collect::<Vec<_>>();
            if let Err(code) = apply_lane_transition(
                &mut next,
                &snapshot.dispatch,
                &resolutions,
                None,
                AgentDispatchState::Open,
                Some(LaneWorkUpdate {
                    work_item_id: work_item_id.to_string(),
                    demand,
                }),
                &mut transitions,
            ) {
                return rejected(snapshot, code);
            }
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
        .state = if matches!(settlement, Settlement::Interrupted { .. }) {
        ActivationState::Interrupted
    } else {
        ActivationState::Settled
    };
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
        Settlement::Continue
        | Settlement::Yield
        | Settlement::Complete { continuation: None }
        | Settlement::Interrupted { .. } => {
            if !lifecycle_nudge {
                let resolutions = current_lane_resolution(
                    snapshot,
                    consumed_wait_id.as_deref(),
                    "consumed->resolved",
                )
                .into_iter()
                .collect::<Vec<_>>();
                if let Err(code) = apply_lane_transition(
                    &mut next,
                    &snapshot.dispatch,
                    &resolutions,
                    None,
                    AgentDispatchState::Open,
                    None,
                    &mut transitions,
                ) {
                    return rejected(snapshot, code);
                }
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
            if let Some(existing_wait) = snapshot.waits.get(&wait.id) {
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
            let mut resolutions = current_lane_resolution(
                snapshot,
                consumed_wait_id.as_deref(),
                "consumed->resolved",
            )
            .into_iter()
            .collect::<Vec<_>>();
            if lifecycle_nudge {
                for (existing_wait_id, record) in &snapshot.waits {
                    if existing_wait_id == &wait.id {
                        continue;
                    }
                    let generation = record
                        .generations
                        .get(&record.current_generation)
                        .expect("current wait generation exists");
                    if generation.owner == owner
                        && matches!(generation.state, WaitState::Active | WaitState::Triggered)
                    {
                        resolutions.push(LaneWaitResolution {
                            wait: WaitIdentity {
                                id: existing_wait_id.clone(),
                                generation: record.current_generation,
                            },
                            expected: generation.clone(),
                            reason: "resolved_by_lifecycle_rearm",
                        });
                    }
                }
            }
            let arm = LaneWaitArm {
                wait: wait.clone(),
                owner: owner.clone(),
            };
            let target_dispatch = match mode {
                WaitMode::AwaitThis => AgentDispatchState::Awaiting { wait: wait.clone() },
                WaitMode::AcceptScheduling => AgentDispatchState::Open,
            };
            if let Err(code) = apply_lane_transition(
                &mut next,
                &snapshot.dispatch,
                &resolutions,
                Some(&arm),
                target_dispatch,
                None,
                &mut transitions,
            ) {
                return rejected(snapshot, code);
            }
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
        .state = if matches!(settlement, Settlement::Interrupted { .. }) {
        ActivationState::Interrupted
    } else {
        ActivationState::Settled
    };
    Outcome {
        decision: Decision::Settled,
        transitions,
        diagnostics: Vec::new(),
        snapshot: next,
    }
}

#[derive(Debug, Clone)]
struct LaneWaitResolution {
    wait: WaitIdentity,
    expected: WaitGenerationRecord,
    reason: &'static str,
}

#[derive(Debug, Clone)]
struct LaneWaitArm {
    wait: WaitIdentity,
    owner: SchedulerOwner,
}

#[derive(Debug, Clone)]
struct LaneWorkUpdate {
    work_item_id: String,
    demand: WorkDemand,
}

fn current_lane_resolution(
    snapshot: &Snapshot,
    wait_id: Option<&str>,
    reason: &'static str,
) -> Option<LaneWaitResolution> {
    let wait_id = wait_id?;
    let wait = snapshot.waits.get(wait_id)?;
    let expected = wait.generations.get(&wait.current_generation)?.clone();
    Some(LaneWaitResolution {
        wait: WaitIdentity {
            id: wait_id.to_string(),
            generation: wait.current_generation,
        },
        expected,
        reason,
    })
}

fn apply_lane_transition(
    snapshot: &mut Snapshot,
    expected_dispatch: &AgentDispatchState,
    resolutions: &[LaneWaitResolution],
    arm: Option<&LaneWaitArm>,
    target_dispatch: AgentDispatchState,
    work_update: Option<LaneWorkUpdate>,
    transitions: &mut Vec<String>,
) -> Result<(), &'static str> {
    if &snapshot.dispatch != expected_dispatch {
        return Err("lane_transition_source_dispatch_changed");
    }
    for resolution in resolutions {
        let Some(wait) = snapshot.waits.get(&resolution.wait.id) else {
            return Err("lane_transition_source_wait_missing");
        };
        if wait.current_generation != resolution.wait.generation
            || wait.generations.get(&resolution.wait.generation) != Some(&resolution.expected)
        {
            return Err("lane_transition_source_wait_changed");
        }
    }
    if let Some(arm) = arm {
        if let Some(wait) = snapshot.waits.get(&arm.wait.id) {
            let Some(current) = wait.generations.get(&wait.current_generation) else {
                return Err("lane_transition_target_wait_missing_generation");
            };
            let current_is_resolved_by_transition = resolutions.iter().any(|resolution| {
                resolution.wait.id == arm.wait.id
                    && resolution.wait.generation == wait.current_generation
            });
            if current.owner != arm.owner
                || wait.current_generation >= arm.wait.generation
                || wait.generations.contains_key(&arm.wait.generation)
                || wait
                    .generations
                    .keys()
                    .any(|generation| *generation > wait.current_generation)
                || (current.state != WaitState::Resolved && !current_is_resolved_by_transition)
            {
                return Err("lane_transition_target_wait_conflict");
            }
        }
    }
    if let AgentDispatchState::Awaiting { wait } = &target_dispatch {
        if !arm.is_some_and(|arm| arm.wait == *wait) {
            return Err("lane_transition_target_dispatch_mismatch");
        }
    }
    if let (Some(arm), Some(work_update)) = (arm, work_update.as_ref()) {
        if let SchedulerOwner::WorkItem { work_item_id } = &arm.owner {
            if work_item_id != &work_update.work_item_id
                || work_update.demand.scheduling_generation != arm.wait.generation
                || work_update.demand.status
                    != (WorkStatus::Waiting {
                        wait_id: arm.wait.id.clone(),
                    })
            {
                return Err("lane_transition_work_projection_mismatch");
            }
        }
    }

    for resolution in resolutions {
        let generation = snapshot
            .waits
            .get_mut(&resolution.wait.id)
            .expect("validated lane source wait exists")
            .generations
            .get_mut(&resolution.wait.generation)
            .expect("validated lane source wait generation exists");
        generation.state = WaitState::Resolved;
        generation.trigger = None;
        generation.consuming_activation_id = None;
        transitions.push(format!(
            "wait:{}:generation:{}:{}",
            resolution.wait.id, resolution.wait.generation, resolution.reason
        ));
    }
    if let Some(work_update) = work_update {
        snapshot
            .work
            .insert(work_update.work_item_id, work_update.demand);
    }
    if let Some(arm) = arm {
        let generation = WaitGenerationRecord {
            owner: arm.owner.clone(),
            state: WaitState::Active,
            trigger: None,
            consuming_activation_id: None,
        };
        if let Some(wait) = snapshot.waits.get_mut(&arm.wait.id) {
            let previous_generation = wait.current_generation;
            wait.current_generation = arm.wait.generation;
            wait.generations.insert(arm.wait.generation, generation);
            transitions.push(format!(
                "wait:{}:generation:{previous_generation}->{}:active",
                arm.wait.id, arm.wait.generation
            ));
        } else {
            snapshot.waits.insert(
                arm.wait.id.clone(),
                WaitRecord {
                    current_generation: arm.wait.generation,
                    generations: BTreeMap::from([(arm.wait.generation, generation)]),
                },
            );
            transitions.push(format!(
                "wait:{}:generation:{}:created:active",
                arm.wait.id, arm.wait.generation
            ));
        }
    }
    set_dispatch_state(snapshot, target_dispatch);
    Ok(())
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

pub fn assert_invariants(snapshot: &Snapshot) -> Result<(), String> {
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
        let event = lower_admit_activation(command)
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
            | AdmissionCause::LifecycleExternalNudge { .. }
            | AdmissionCause::InternalFollowup { .. } => None,
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
            || !idempotency_keys.insert(command.activation.idempotency_key.as_str())
        {
            return Err("canonical activation admission record is invalid".into());
        }
    }
    if snapshot.admitted_generations != canonical_admission_fences {
        return Err(format!(
            "canonical admission fences disagree with activation admissions: persisted={:?}, canonical={canonical_admission_fences:?}",
            snapshot.admitted_generations
        ));
    }
    let mut authority_ids = BTreeSet::new();
    if snapshot
        .activation_admissions
        .values()
        .any(|command| !authority_ids.insert(command.authority_id.as_str()))
    {
        return Err("canonical activation admissions reuse authority identity".into());
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
        let expected_activation_state = if matches!(lowered, Settlement::Interrupted { .. }) {
            ActivationState::Interrupted
        } else {
            ActivationState::Settled
        };
        if settlement_id != &settlement.id
            || activation.state != expected_activation_state
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
                | Settlement::Complete { continuation: None }
                | Settlement::Interrupted { .. } => {}
                Settlement::Wait {
                    wait,
                    mode: _,
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
            Settlement::Continue | Settlement::Yield | Settlement::Interrupted { .. } => {
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
                mode: _,
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
            ActivationState::Interrupted => {
                return Err(
                    "interrupted activation cannot have a missing-settlement record".into(),
                );
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
            ActivationState::Settled
            | ActivationState::Interrupted
            | ActivationState::SettlementMissing => 1,
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
            ActivationState::Settled | ActivationState::Interrupted => {
                if matches!(
                    snapshot.slot,
                    ActivationSlot::Running {
                        activation_id: ref slot_activation_id,
                        ..
                    } if slot_activation_id == activation_id
                ) {
                    return Err("terminal activation still owns the slot".into());
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

fn rejected(snapshot: &Snapshot, diagnostic: &str) -> Outcome {
    Outcome {
        decision: Decision::Rejected,
        transitions: Vec::new(),
        diagnostics: vec![diagnostic.to_string()],
        snapshot: snapshot.clone(),
    }
}

#[cfg(test)]
mod wire_compatibility_tests {

    use super::{
        activation_provenance_matches_cause, ActivationBinding, ActivationCause,
        ActivationInputAttachment, ActivationOrigin, ActivationProvenance, ActivationTrust,
        SchedulerOwner, WaitGenerationRecord, WaitState,
    };

    #[test]
    fn terminal_task_result_can_authorize_lifecycle_nudge() {
        assert!(activation_provenance_matches_cause(
            &ActivationProvenance {
                origin: ActivationOrigin::Task,
                trust: ActivationTrust::RuntimeInstruction,
                source_id: "message-task-result".into(),
                correlation_id: None,
                causation_id: None,
            },
            &ActivationCause::LifecycleExternalNudge {
                message_id: "message-task-result".into(),
            },
        ));
    }

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
