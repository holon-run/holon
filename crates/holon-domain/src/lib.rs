//! Shared domain values used across Holon layers.

#![forbid(unsafe_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

pub mod calendar;
pub mod decision_telemetry;
pub mod execution_protocol;
pub mod execution_types;
pub mod ids;
pub mod model_values;
pub mod runtime_event;
pub mod scheduler;
pub mod timezone;

mod agent;
mod agent_observation;
pub mod agent_records;
mod audit;
mod brief;
mod image_contract;
mod input;
mod instructions;
mod media;
mod memory;
mod message;
mod model;
mod operator;
mod runtime_error;
mod serde_helpers;
mod skills;
mod state;
mod task;
mod templates;
mod tool;
mod tool_error;
mod trace;
mod transcript;
mod turn;
mod wake;
mod wire;
pub mod work_item;
mod workspace;
mod workspace_identity;

pub use agent::*;
pub use agent_observation::*;
pub use agent_records::*;
pub use audit::*;
pub use brief::*;
pub use input::*;
pub use instructions::*;
pub use memory::*;
pub use message::*;
pub use model::*;
pub use operator::*;
pub use runtime_error::*;
pub use skills::*;
pub use state::*;
pub use task::*;
pub use templates::*;
pub use tool::*;
pub use transcript::*;
pub use turn::*;
pub use wake::*;
pub use work_item::*;
pub use workspace::*;
pub use workspace_identity::*;

pub use image_contract::{
    parse_image_dimensions, ImageConversionMetadata, ImageGenerationApi, ImageGenerationContract,
    ImageGenerationParameterError, ImageGenerationParameters, ImageParameterContract,
    ImageParameterSupport, ImageSizeConstraints, ResolvedImageGenerationParameters,
};
pub use media::*;
pub use tool_error::ToolError;
pub use trace::{TraceContext, TraceContextError};
pub use wire::*;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct TurnKey {
    pub turn_index: u64,
    pub turn_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct ActivityKey {
    pub event_seq: u64,
    pub activity_id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContinuationTriggerKind {
    OperatorInput,
    TaskResult,
    ExternalEvent,
    TimerFire,
    InternalFollowup,
    SystemTick,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    OperatorPrompt,
    ChannelEvent,
    WebhookEvent,
    CallbackEvent,
    TimerTick,
    SystemTick,
    TaskResult,
    TaskStatus,
    Control,
    BriefAck,
    BriefResult,
    InternalFollowup,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TurnTerminalKind {
    Completed,
    Aborted,
    Interrupted,
    BaselineOverBudget,
    DeferredToFallback,
    ProviderFailedNeedsRecovery,
}

impl TurnTerminalKind {
    pub fn is_failure(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

#[cfg(test)]
mod tests;
