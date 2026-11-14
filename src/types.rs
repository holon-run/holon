//! Shared runtime types and their compatibility imports.

pub use holon_domain::{ContinuationTriggerKind, MessageKind, TurnTerminalKind};

pub use crate::domain::{agent::*, agent_home_workspace_id, work_item::*, AGENT_HOME_WORKSPACE_ID};
pub use crate::runtime::agent_observation::{
    AgentLineageChildrenSnapshot, ChildAgentBlockedReason, ChildAgentObservabilitySnapshot,
    ChildAgentPhase,
};
pub use crate::tool::tools::get_agent::{
    GetAgentCompactResult, GetAgentResult, SubagentCleanupCandidate,
};

mod agent;
mod audit;
mod brief;
mod instructions;
mod memory;
mod message;
mod model;
mod operator;
mod serde_helpers;
mod skills;
mod state;
mod task;
mod templates;
mod tool;
mod transcript;
mod turn;
mod wake;
mod workspace;

pub use agent::*;
pub use audit::*;
pub use brief::*;
pub use instructions::*;
pub use memory::*;
pub use message::*;
pub use model::*;
pub use operator::*;
pub use skills::*;
pub use state::*;
pub use task::*;
pub use templates::*;
pub use tool::*;
pub use transcript::*;
pub use turn::*;
pub use wake::*;
pub use workspace::*;

#[cfg(holon_lib_test_shard = "d")]
#[cfg(test)]
mod tests;
