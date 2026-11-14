//! Shared domain values used across Holon layers.

#![forbid(unsafe_code)]

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod input;
mod message;
mod runtime_error;
mod turn;

pub use input::{IdleInputBatchItem, IdleInputBatchRecord};
pub use message::{BriefAttachment, MessageBody, MessageOrigin, Priority};
pub use runtime_error::{RuntimeErrorContext, RuntimeErrorDescriptor, RuntimeErrorDomain};
pub use turn::{
    TurnNoBriefReason, TurnOwner, TurnReplayProvenance, TurnTerminalCheckpointRecord,
    TurnTerminalRecord, TurnTerminalSummary,
};

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
