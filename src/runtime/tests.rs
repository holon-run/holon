use std::sync::Arc;

use async_trait::async_trait;
use tempfile::{tempdir, TempDir};
use tokio::sync::Mutex;

use crate::{
    config::AppConfig,
    context::ContextConfig,
    host::RuntimeHost,
    prompt::{render_section, PromptSection, PromptStability},
    provider::{
        provider_turn_error, ProviderAttemptOutcome, ProviderAttemptRecord,
        ProviderAttemptTimeline, ProviderTransportDiagnostics, ProviderTurnResponse,
        ReqwestTransportDiagnostics, StubProvider,
    },
    storage::AppStorage,
    system::{ExecutionProfile, ExecutionSnapshot},
    types::{
        AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset, AgentRegistryStatus,
        AgentState, AgentStatus, AgentVisibility, AuthorityClass, BriefKind, BriefRecord,
        CallbackDeliveryMode, ClosureOutcome, ContinuationClass, ContinuationTriggerKind,
        LoadedAgentsMd, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
        PendingWakeHint, Priority, TaskOutputRetrievalStatus, TaskRecord, TaskRecoverySpec,
        TaskStatus, TimerRecord, TimerStatus, TokenUsage, TurnTerminalKind, TurnTerminalRecord,
        WaitingIntentStatus, WaitingReason, WorkItemRecord, WorkItemState, WorkReactivationMode,
        WorkspaceEntry,
    },
};

use super::*;

mod support;
mod workspace;
mod state;
mod visibility;
mod turns;
mod output;
