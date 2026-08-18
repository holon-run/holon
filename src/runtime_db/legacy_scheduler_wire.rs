//! Deserialization-only compatibility types for retired scheduler payloads.
//!
//! Published databases may still contain these JSON shapes while migrating to
//! the execution protocol. Keep them private to `runtime_db`; they are not a
//! current scheduler contract.

use std::collections::BTreeSet;

use serde::{de::Error as _, Deserialize, Deserializer, Serialize};

use crate::domain::scheduler::SchedulerOwner;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct WorkDemand {
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
pub(super) enum WorkStatus {
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
pub(super) struct YieldContinuationRecord {
    pub continuation_id: String,
    pub source_work_item_id: String,
    pub source_generation: u64,
    pub target_work_item_id: String,
    pub target_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AdmitActivationCommand {
    pub authority_id: String,
    pub activation: AgentActivation,
    pub expected_scheduling_generation: u64,
    pub expected_dispatch_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AgentActivation {
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
pub(super) enum ActivationLifecycleState {
    Admitted,
    Running,
    Settled,
    Interrupted,
    Cancelled,
    SettlementMissing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ActivationProvenance {
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
pub(super) enum ActivationOrigin {
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
pub(super) enum ActivationTrust {
    OperatorInstruction,
    RuntimeInstruction,
    IntegrationSignal,
    ExternalEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ActivationCause {
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
pub(super) struct WaitResumeClaim {
    pub wait_id: String,
    pub wait_generation: u64,
    pub trigger_id: String,
    pub trigger_generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(super) enum ActivationBinding {
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
pub(super) enum ActivationPriority {
    Background,
    Normal,
    Next,
    Interject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum PreemptionPolicy {
    NonPreemptive,
    AllowOperatorInterjection,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ActivationInputAttachment {
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

#[cfg(test)]
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

#[cfg(test)]
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
