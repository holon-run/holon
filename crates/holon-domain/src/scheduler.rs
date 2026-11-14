//! Compatibility-neutral scheduler domain types shared by canonical execution
//! and legacy protocol adapters.

use serde::{Deserialize, Serialize};

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
