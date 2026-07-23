mod bootstrap;
mod callback;
mod clock;
mod closure;
mod command_task;
mod continuation;
mod delivery;
mod failure;
mod first_run_intro;
mod lifecycle;
mod memory_refresh;
mod message_dispatch;
mod operator;
mod operator_dispatch;
mod provider_turn;
mod scheduler;
mod scheduler_acceptance;
mod scheduler_executor;
mod subagent;
mod task_state_reducer;
mod task_supervisor;
mod tasks;
#[cfg(test)]
mod test_util;
mod turn;
mod waiting;
pub(crate) mod workspace;
pub(crate) mod workspace_control;
mod worktree;

pub use first_run_intro::maybe_enqueue_first_run_intro;
pub(crate) use lifecycle::LightweightAgentStateProjection;
pub use scheduler_acceptance::{
    seed_scheduler_terminal_recovery_fixture, SchedulerTerminalRecoveryFixture,
};
pub use tasks::{
    PickedWorkItem, WorkItemContinuationSummary, WorkItemFocusTransition,
    WorkItemFocusTransitionWarning,
};
pub(crate) use waiting::{WaitForScope, WaitForWakeKind};

#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::{
    collections::{hash_map::Entry, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use anyhow::{anyhow, Context, Result};
use arc_swap::ArcSwap;
use bootstrap::ConfigSnapshot;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, Notify, RwLock};
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

#[cfg(test)]
use crate::provider::{ConversationMessage, ProviderTurnRequest};
use crate::{
    agent_memory::load_agent_memory,
    agent_template::discover_agent_templates_catalog,
    agents_md::load_agents_md,
    brief,
    config::RuntimeModelCatalog,
    context::{sync_agent_message_count, ContextConfig},
    host::RuntimeHostBridge,
    ingress::WakeDisposition,
    memory::{refresh_episode_memory, refresh_working_memory},
    prompt::{
        build_effective_prompt_with_apply_patch_surface,
        build_effective_prompt_with_apply_patch_surface_and_default_external_ingress,
        EffectivePrompt,
    },
    provider::{
        provider_attempt_timeline, AgentProvider, ModelBlock, ProviderBuiltinWebSearchCapability,
        ProviderNativeWebSearchKind, ProviderNativeWebSearchRequest,
    },
    queue::RuntimeQueue,
    runtime_db::{
        transitions::{
            PostCommitWarning, TransitionApplyResult, TransitionCommit, TransitionFaultPoint,
        },
        RuntimeDb,
    },
    runtime_error::describe_runtime_error,
    runtime_event::RuntimeEventKind,
    skills::{
        effective_skill_root_registrations, find_skill_by_entrypoint, find_skill_by_script_path,
        skills_runtime_view_from_catalog, SkillVisibility,
    },
    storage::{to_json_value, AppStorage, PollActivityMarker},
    system::{
        EffectiveExecution, ExecutionScopeKind, ExecutionSnapshot, LocalSystem,
        WorkspaceAccessMode, WorkspaceProjectionKind, WorkspaceView,
    },
    tool::{ToolRegistry, ToolResult},
    types::LoadedAgentMemory,
    types::{
        ActiveWorkspaceEntry, AdmissionContext, AgentIdentityView, AgentKind,
        AgentModelOverrideAuditEvent, AgentModelSource, AgentModelState, AgentState,
        AgentStateChangedEvent, AgentStatus, AgentSummary, AuditEvent, AuthorityClass,
        BriefCreatedAuditEvent, BriefRecord, CallbackDeliveryMode, CallbackDeliveryPayload,
        CallbackDeliveryResult, CallbackIngressDisposition, ClosureDecision,
        ContinuationResolution, ControlAction, ExecCommandBatchItemStatus, ExecCommandBatchResult,
        ExternalTriggerCapability, ExternalTriggerRecord, ExternalTriggerScope,
        ExternalTriggerStatus, ExternalTriggerSummary, LoadedAgentsMd, MessageBody,
        MessageDeliverySurface, MessageEnvelope, MessageKind, MessageLifecycleAuditEvent,
        MessageOrigin, PendingWakeHint, Priority, QueueEntryRecord, QueueEntryStatus,
        RuntimeFailurePhase, RuntimeFailureSummary, RuntimePosture, SkillActivationSource,
        SkillActivationState, SkillCatalogEntry, SkillLoadReason, SkillsRuntimeView, TaskKind,
        TaskLifecycleAuditEvent, TaskRecord, TaskRecoverySpec, TaskStatus, TimerRecord,
        TimerStatus, ToolExecutionRecord, TranscriptEntry, TranscriptEntryKind,
        ViewImageObservation, WaitingReason, WorkItemExecutionBinding, WorkItemLifecycleAuditEvent,
        WorkspaceEntry, AGENT_HOME_WORKSPACE_ID,
    },
    web::{WebConfig, WebProviderKind},
};
use command_task::ManagedTaskHandle;
use continuation::{resolve_continuation, ContinuationTrigger};
#[cfg(test)]
use subagent::sanitize_subagent_result;
use turn::LoopControlOptions;

#[derive(Debug, Clone)]
pub(super) struct WorkItemCompletionReportPromotion {
    pub(super) record: crate::types::WorkItemRecord,
    pub(super) brief_id: String,
}

#[derive(Debug, Clone)]
pub(super) enum WorkItemCompletionReportPromotionOutcome {
    /// Completion changed the WorkItem state, but did not create a new
    /// user-facing report for terminal delivery.
    Unchanged(crate::types::WorkItemRecord),
    /// Completion promoted the assistant's same-round report into the
    /// WorkItem's canonical result brief.
    Promoted(WorkItemCompletionReportPromotion),
}

impl WorkItemCompletionReportPromotionOutcome {
    pub(super) fn into_record(self) -> crate::types::WorkItemRecord {
        match self {
            Self::Unchanged(record) => record,
            Self::Promoted(promotion) => promotion.record,
        }
    }
}

#[derive(Debug, Clone)]
struct WorktreeSubagentResult {
    text: String,
    worktree_path: PathBuf,
    worktree_branch: String,
    changed_files: Vec<String>,
    failed: bool,
}

#[derive(Debug, Clone)]
pub struct ManagedWorktreeSeed {
    pub original_cwd: PathBuf,
    pub original_branch: String,
    pub worktree_path: PathBuf,
    pub worktree_branch: String,
}

#[derive(Debug, Clone)]
pub enum InitialWorkspaceBinding {
    Detached,
    Anchor(PathBuf),
    Entry(WorkspaceEntry),
}

impl From<PathBuf> for InitialWorkspaceBinding {
    fn from(value: PathBuf) -> Self {
        Self::Anchor(value)
    }
}

impl From<WorkspaceEntry> for InitialWorkspaceBinding {
    fn from(value: WorkspaceEntry) -> Self {
        Self::Entry(value)
    }
}

impl From<Option<WorkspaceEntry>> for InitialWorkspaceBinding {
    fn from(value: Option<WorkspaceEntry>) -> Self {
        match value {
            Some(value) => Self::Entry(value),
            None => Self::Detached,
        }
    }
}

pub(crate) fn agent_model_state_for_catalog(
    model_catalog: &RuntimeModelCatalog,
    base_context_config: &ContextConfig,
    state: &AgentState,
) -> AgentModelState {
    let effective_model = model_catalog.effective_model(state.model_override.as_ref());
    let active_model = state
        .last_requested_model
        .as_ref()
        .filter(|requested| *requested == &effective_model)
        .and_then(|_| state.last_active_model.clone())
        .unwrap_or_else(|| effective_model.clone());
    let fallback_active = active_model != effective_model;
    let effective_chain = model_catalog.provider_chain_for_turn(
        state.model_override.as_ref(),
        state.pending_fallback_model.as_ref(),
    );
    let resolved_policy =
        model_catalog.resolved_model_policy(base_context_config, state.model_override.as_ref());
    AgentModelState {
        source: if state.model_override.is_some() {
            AgentModelSource::AgentOverride
        } else {
            AgentModelSource::RuntimeDefault
        },
        runtime_default_model: model_catalog.default_model.clone(),
        effective_model: effective_model.clone(),
        requested_model: Some(effective_model),
        active_model: Some(active_model),
        fallback_active,
        effective_fallback_models: effective_chain.into_iter().skip(1).collect(),
        override_model: state.model_override.clone(),
        override_reasoning_effort: state.model_override_reasoning_effort.clone(),
        resolved_policy,
    }
}

pub(crate) fn lightweight_agent_list_waiting_reason(agent: &AgentState) -> Option<WaitingReason> {
    match agent.status {
        AgentStatus::AwaitingTask => Some(WaitingReason::AwaitingTaskResult),
        _ => None,
    }
}

#[derive(Clone)]
pub struct RuntimeHandle {
    inner: Arc<RuntimeInner>,
}

struct RuntimeInner {
    agent: Mutex<RuntimeAgent>,
    projection_cache: Mutex<AgentRuntimeProjectionCache>,
    object_query_cache: Arc<crate::object_query_cache::ObjectQueryCache>,
    notify: Notify,
    storage: AppStorage,
    runtime_db: RuntimeDb,
    clock: Arc<dyn clock::Clock>,
    provider: RwLock<Arc<dyn AgentProvider>>,
    context_config: RwLock<ContextConfig>,
    config_snapshot: ArcSwap<ConfigSnapshot>,
    builtin_web_search_probe_cache:
        Mutex<HashMap<BuiltinWebSearchProbeKey, BuiltinWebSearchProbeCacheEntry>>,
    view_image_observation_cache:
        Mutex<HashMap<ViewImageObservationCacheKey, ViewImageObservation>>,
    model_discovery_refreshes: Mutex<HashSet<crate::config::ProviderId>>,
    callback_base_url: String,
    tools: ToolRegistry,
    system: Arc<LocalSystem>,
    default_agent_id: String,
    host_bridge: Option<RuntimeHostBridge>,
    task_handles: Mutex<HashMap<String, ManagedTaskHandle>>,
    recovered_tasks: Mutex<Option<Vec<TaskRecord>>>,
    recovered_timers: Mutex<Option<Vec<TimerRecord>>>,
    suppress_next_continue_active_tick: Mutex<bool>,
    shutdown_requested: AtomicBool,
    scheduler_protocol_production_commands_enabled: AtomicBool,
    #[cfg(test)]
    transition_faults: StdMutex<std::collections::VecDeque<TransitionFaultPoint>>,
    #[cfg(test)]
    task_transition_conflicts_remaining: AtomicUsize,
    #[cfg(test)]
    omit_next_scheduler_claim_shadow_comparison: AtomicBool,
    #[cfg(test)]
    fail_after_next_runtime_claim: AtomicBool,
    #[cfg(test)]
    transition_warnings: StdMutex<Vec<PostCommitWarning>>,
}

const SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS_ENV: &str =
    "HOLON_SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS";
const SCHEDULER_ACCEPTANCE_FIXTURES_ENV: &str = "HOLON_SCHEDULER_ACCEPTANCE_FIXTURES";

fn scheduler_protocol_production_commands_enabled_from_env() -> Result<bool> {
    boolean_env(SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS_ENV).map(|value| value.unwrap_or(false))
}

fn boolean_env(name: &str) -> Result<Option<bool>> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(None);
    };
    let value = match value.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => true,
        "0" | "false" | "no" | "off" => false,
        _ => return Err(anyhow!("{name} expects a boolean")),
    };
    Ok(Some(value))
}

pub fn require_scheduler_acceptance_fixtures_enabled() -> Result<()> {
    if boolean_env(SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS_ENV)? != Some(true) {
        return Err(anyhow!(
            "scheduler acceptance fixtures require {SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS_ENV}=true"
        ));
    }
    if boolean_env(SCHEDULER_ACCEPTANCE_FIXTURES_ENV)? != Some(true) {
        return Err(anyhow!(
            "scheduler acceptance fixtures require {SCHEDULER_ACCEPTANCE_FIXTURES_ENV}=true"
        ));
    }
    Ok(())
}

#[cfg(test)]
fn scheduler_acceptance_fixtures_enabled_from_values(
    production_commands: Option<&str>,
    acceptance_fixtures: Option<&str>,
) -> Result<bool> {
    fn parse(name: &str, value: Option<&str>) -> Result<Option<bool>> {
        let Some(value) = value else {
            return Ok(None);
        };
        match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Ok(Some(true)),
            "0" | "false" | "no" | "off" => Ok(Some(false)),
            _ => Err(anyhow!("{name} expects a boolean")),
        }
    }
    Ok(parse(
        SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS_ENV,
        production_commands,
    )? == Some(true)
        && parse(SCHEDULER_ACCEPTANCE_FIXTURES_ENV, acceptance_fixtures)? == Some(true))
}

#[cfg(test)]
mod scheduler_acceptance_gate_tests {
    use super::*;

    #[test]
    fn scheduler_acceptance_gate_requires_both_explicit_flags() {
        assert!(
            scheduler_acceptance_fixtures_enabled_from_values(Some("true"), Some("true")).unwrap()
        );
        assert!(!scheduler_acceptance_fixtures_enabled_from_values(Some("true"), None).unwrap());
        assert!(!scheduler_acceptance_fixtures_enabled_from_values(None, Some("true")).unwrap());
        assert!(
            !scheduler_acceptance_fixtures_enabled_from_values(Some("false"), Some("true"))
                .unwrap()
        );
    }

    #[test]
    fn scheduler_acceptance_gate_rejects_invalid_boolean() {
        assert!(
            scheduler_acceptance_fixtures_enabled_from_values(Some("sometimes"), Some("true"))
                .unwrap_err()
                .to_string()
                .contains(SCHEDULER_PROTOCOL_PRODUCTION_COMMANDS_ENV)
        );
    }
}

fn canonical_settlement_id(message_id: &str) -> String {
    format!("settlement:message:{message_id}")
}

fn canonical_missing_settlement_id(message_id: &str) -> String {
    format!("missing-settlement:message:{message_id}")
}

fn canonical_queue_settlement_commands_from_facts(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    record: &QueueEntryRecord,
) -> Result<Vec<crate::domain::scheduler_protocol::ProtocolCommand>> {
    let Some(message) = storage.read_message_by_id(&record.message_id)? else {
        return Ok(Vec::new());
    };
    use crate::domain::scheduler_protocol::{
        ActivationDisposition, ActivationSettlement, AgentDispatchDisposition,
        MissingSettlementRecord, ProtocolCommand, SettleActivationCommand, WaitIdentity,
    };

    let Some(snapshot) = runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot_if_initialized(&record.agent_id)?
    else {
        return Ok(Vec::new());
    };
    let activation_id = scheduler_executor::canonical_activation_id(&record.message_id);
    let Some(activation) = snapshot.activations.get(&activation_id) else {
        return Ok(Vec::new());
    };
    let work_item_id = activation.work_item_id.clone();
    let work_queue = storage.work_queue_prompt_projection()?;
    let scheduling_state = work_queue
        .items
        .iter()
        .find(|candidate| candidate.id == work_item_id)
        .map(|candidate| candidate.scheduling_state);
    let missing_settlement = || {
        ProtocolCommand::RecordMissingSettlement(MissingSettlementRecord {
            id: canonical_missing_settlement_id(&record.message_id),
            activation_id: activation_id.clone(),
            created_at: record.updated_at.to_rfc3339(),
        })
    };

    let command = if record.status == QueueEntryStatus::Processed {
        match scheduling_state {
            Some(crate::types::WorkItemSchedulingState::Runnable) => {
                ProtocolCommand::SettleActivation(SettleActivationCommand {
                    settlement: ActivationSettlement {
                        id: canonical_settlement_id(&record.message_id),
                        activation_id,
                        turn_terminal: message.turn_id.clone(),
                        disposition: ActivationDisposition::WorkContinues,
                        agent_dispatch: AgentDispatchDisposition::Open,
                        operator_delivery: None,
                        evidence: vec![
                            format!("message:{}", record.message_id),
                            format!("work_item:{work_item_id}"),
                        ],
                        created_at: record.updated_at.to_rfc3339(),
                    },
                })
            }
            Some(crate::types::WorkItemSchedulingState::Completed) => {
                let Some(work_item) = runtime_db.work_items().latest(&work_item_id)? else {
                    return Ok(vec![missing_settlement()]);
                };
                let Some(turn_terminal) = message.turn_id.clone() else {
                    return Ok(vec![missing_settlement()]);
                };
                let Some(completion_intent) = work_item.completion_intent.as_ref() else {
                    return Ok(vec![missing_settlement()]);
                };
                let Some(admission) = snapshot.activation_admissions.get(&activation_id) else {
                    return Ok(vec![missing_settlement()]);
                };
                if !(completion_intent.work_item_id == work_item_id
                    && completion_intent.source_activation_id.as_deref()
                        == Some(activation_id.as_str())
                    && completion_intent.source_message_id.as_deref()
                        == Some(record.message_id.as_str())
                    && completion_intent.source_turn_id.as_deref() == Some(turn_terminal.as_str())
                    && admission.activation.source_revision
                        == Some(completion_intent.expected_work_revision))
                {
                    return Ok(vec![missing_settlement()]);
                }
                let Some(result_brief_id) =
                    completion_intent
                        .result_brief_id
                        .as_deref()
                        .filter(|result_brief_id| {
                            work_item.result_brief_id.as_deref() == Some(*result_brief_id)
                        })
                else {
                    return Ok(vec![missing_settlement()]);
                };
                let Some(brief) = storage.read_brief_by_id(result_brief_id)?.filter(|brief| {
                    brief.kind.is_success()
                        && brief.work_item_id.as_deref() == Some(work_item_id.as_str())
                        && brief.turn_id.as_deref() == Some(turn_terminal.as_str())
                        && brief.related_message_id.as_deref() == Some(record.message_id.as_str())
                        && !brief.text.trim().is_empty()
                }) else {
                    return Ok(vec![missing_settlement()]);
                };
                ProtocolCommand::SettleActivation(SettleActivationCommand {
                    settlement: ActivationSettlement {
                        id: canonical_settlement_id(&record.message_id),
                        activation_id,
                        turn_terminal: Some(turn_terminal.clone()),
                        disposition: ActivationDisposition::WorkCompleted { continuation: None },
                        agent_dispatch: AgentDispatchDisposition::Open,
                        operator_delivery: Some(brief.id.clone()),
                        evidence: vec![
                            format!("message:{}", record.message_id),
                            format!("work_item:{work_item_id}"),
                            format!("turn:{turn_terminal}"),
                            format!("brief:{}", brief.id),
                        ],
                        created_at: record.updated_at.to_rfc3339(),
                    },
                })
            }
            Some(
                crate::types::WorkItemSchedulingState::WaitingOperator
                | crate::types::WorkItemSchedulingState::WaitingTask
                | crate::types::WorkItemSchedulingState::WaitingExternal
                | crate::types::WorkItemSchedulingState::WaitingTimer
                | crate::types::WorkItemSchedulingState::WaitingSystem,
            ) => {
                let active_waits = storage
                    .active_wait_conditions_for_work_item(&record.agent_id, &work_item_id)?;
                let [active_wait] = active_waits.as_slice() else {
                    return Ok(vec![missing_settlement()]);
                };
                let generation = activation
                    .admitted_generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("canonical wait generation overflow"))?;
                ProtocolCommand::SettleActivation(SettleActivationCommand {
                    settlement: ActivationSettlement {
                        id: canonical_settlement_id(&record.message_id),
                        activation_id,
                        turn_terminal: message.turn_id.clone(),
                        disposition: ActivationDisposition::WorkWaits {
                            wait: WaitIdentity {
                                id: active_wait.id.clone(),
                                generation,
                            },
                        },
                        agent_dispatch: AgentDispatchDisposition::Open,
                        operator_delivery: None,
                        evidence: vec![
                            format!("message:{}", record.message_id),
                            format!("work_item:{work_item_id}"),
                            format!("wait:{}", active_wait.id),
                        ],
                        created_at: record.updated_at.to_rfc3339(),
                    },
                })
            }
            _ => missing_settlement(),
        }
    } else {
        missing_settlement()
    };
    Ok(vec![command])
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerRecoveryReport {
    pub agent_id: String,
    pub partition_initialized: bool,
    pub candidates: Vec<SchedulerRecoveryCandidate>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SchedulerRecoveryCandidate {
    pub message_id: String,
    pub activation_id: String,
    pub work_item_id: Option<String>,
    pub queue_status: QueueEntryStatus,
    pub terminal_turn_id: Option<String>,
    pub eligible: bool,
    pub reason: String,
    pub target_queue_status: Option<QueueEntryStatus>,
    pub evidence: Vec<String>,
    pub proposed_commands: Vec<crate::domain::scheduler_protocol::ProtocolCommand>,
}

pub fn scheduler_recovery_report(
    storage: &AppStorage,
    runtime_db: &RuntimeDb,
    agent_id: &str,
) -> Result<SchedulerRecoveryReport> {
    let Some(snapshot) = runtime_db
        .transitions()
        .load_scheduler_protocol_snapshot_if_initialized(agent_id)?
    else {
        return Ok(SchedulerRecoveryReport {
            agent_id: agent_id.to_string(),
            partition_initialized: false,
            candidates: Vec::new(),
        });
    };
    // This debug-only report intentionally scans retained history so it cannot
    // omit an old recovery candidate; its cost scales with agent history.
    let turns = runtime_db
        .turn_records()
        .recent_for_agent(agent_id, usize::MAX)?;
    let mut entries = runtime_db
        .queue_entries()
        .recent(Some(agent_id), usize::MAX)?
        .into_iter()
        .filter(|entry| entry.status == QueueEntryStatus::Dequeued)
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.message_id.cmp(&right.message_id));
    let mut candidates = Vec::with_capacity(entries.len());

    for entry in entries {
        let activation_id = scheduler_executor::canonical_activation_id(&entry.message_id);
        let mut candidate = SchedulerRecoveryCandidate {
            message_id: entry.message_id.clone(),
            activation_id: activation_id.clone(),
            work_item_id: None,
            queue_status: entry.status.clone(),
            terminal_turn_id: None,
            eligible: false,
            reason: "activation_missing".into(),
            target_queue_status: None,
            evidence: vec!["queue_status=dequeued".into()],
            proposed_commands: Vec::new(),
        };
        let Some(activation) = snapshot.activations.get(&activation_id) else {
            candidates.push(candidate);
            continue;
        };
        candidate.work_item_id = Some(activation.work_item_id.clone());
        candidate
            .evidence
            .push(format!("activation_state={:?}", activation.state));
        candidate.evidence.push(format!(
            "admitted_generation={}",
            activation.admitted_generation
        ));
        let Some(message) = storage.read_message_by_id(&entry.message_id)? else {
            candidate.reason = "message_missing".into();
            candidates.push(candidate);
            continue;
        };
        if !matches!(
            (&message.kind, &message.origin),
            (MessageKind::SystemTick, MessageOrigin::System { subsystem })
                if subsystem == "work_queue"
        ) {
            candidate.reason = "message_not_work_queue_tick".into();
            candidates.push(candidate);
            continue;
        }
        if message.work_item_id.as_deref() != Some(activation.work_item_id.as_str()) {
            candidate.reason = "work_item_binding_mismatch".into();
            candidates.push(candidate);
            continue;
        }

        let terminal_turn = turns.iter().find(|turn| {
            turn.terminal.is_some()
                && turn
                    .trigger
                    .as_ref()
                    .and_then(|trigger| trigger.message_id.as_deref())
                    == Some(entry.message_id.as_str())
                && message.turn_id.as_deref() == Some(turn.turn_id.as_str())
                && turn.current_work_item_id.as_deref() == Some(activation.work_item_id.as_str())
        });
        candidate.terminal_turn_id = terminal_turn.map(|turn| turn.turn_id.clone());
        let terminal_is_completed = terminal_turn.is_some_and(|turn| {
            turn.terminal
                .as_ref()
                .is_some_and(|terminal| terminal.kind == crate::types::TurnTerminalKind::Completed)
        });
        if activation.state == crate::domain::scheduler_protocol::ActivationState::Settled {
            candidate.eligible = true;
            candidate.reason = "canonical_settlement_legacy_queue_pending".into();
            candidate.target_queue_status = Some(if terminal_is_completed {
                QueueEntryStatus::Processed
            } else {
                QueueEntryStatus::Aborted
            });
            candidates.push(candidate);
            continue;
        }
        if activation.state == crate::domain::scheduler_protocol::ActivationState::SettlementMissing
        {
            candidate.eligible = true;
            candidate.reason = "canonical_missing_settlement_legacy_queue_pending".into();
            candidate.target_queue_status = Some(QueueEntryStatus::Aborted);
            candidates.push(candidate);
            continue;
        }
        let mut proposed_entry = entry.clone();
        proposed_entry.status = if terminal_is_completed {
            QueueEntryStatus::Processed
        } else {
            QueueEntryStatus::Aborted
        };
        let mut commands =
            canonical_queue_settlement_commands_from_facts(storage, runtime_db, &proposed_entry)?;
        let settles_from_terminal = matches!(
            commands.as_slice(),
            [crate::domain::scheduler_protocol::ProtocolCommand::SettleActivation(_)]
        );
        if !settles_from_terminal {
            proposed_entry.status = QueueEntryStatus::Aborted;
            commands = canonical_queue_settlement_commands_from_facts(
                storage,
                runtime_db,
                &proposed_entry,
            )?;
        }
        if let Some(diagnostics) = commands.iter().find_map(|command| {
            let outcome = crate::domain::scheduler_protocol::reduce_command(&snapshot, command);
            (outcome.outcome.decision == crate::domain::scheduler_protocol::Decision::Rejected)
                .then(|| outcome.outcome.diagnostics)
        }) {
            candidate.reason = "typed_command_rejected".into();
            candidate.evidence.extend(diagnostics);
            candidate.proposed_commands = commands;
            candidates.push(candidate);
            continue;
        }
        candidate.eligible = true;
        candidate.reason = if settles_from_terminal {
            "terminal_turn_settlement".into()
        } else if terminal_is_completed {
            "terminal_evidence_incomplete".into()
        } else {
            "terminal_turn_missing".into()
        };
        candidate.target_queue_status = Some(proposed_entry.status);
        candidate.proposed_commands = commands;
        candidates.push(candidate);
    }

    Ok(SchedulerRecoveryReport {
        agent_id: agent_id.to_string(),
        partition_initialized: true,
        candidates,
    })
}

fn runtime_error_queue_settlement(
    message_kind: &MessageKind,
    error: &anyhow::Error,
) -> (QueueEntryStatus, &'static str) {
    let retry_task_result = matches!(message_kind, MessageKind::TaskResult)
        && error.chain().any(|source| {
            source
                .downcast_ref::<task_state_reducer::TaskTransitionRetryExhausted>()
                .is_some()
        });
    if retry_task_result {
        (
            QueueEntryStatus::Interrupted,
            "task_transition_retry_exhausted",
        )
    } else {
        (QueueEntryStatus::Aborted, "runtime_error")
    }
}

#[derive(Debug, Clone)]
struct AgentRuntimeProjectionCache {
    agent_id: String,
    tasks: HashMap<String, TaskRecord>,
    work_items: HashMap<String, crate::types::WorkItemRecord>,
    timers: HashMap<String, TimerRecord>,
    external_triggers: HashMap<String, ExternalTriggerRecord>,
}

impl AgentRuntimeProjectionCache {
    fn rebuild(
        agent_id: String,
        tasks: Vec<TaskRecord>,
        work_items: Vec<crate::types::WorkItemRecord>,
        timers: Vec<TimerRecord>,
        external_triggers: Vec<ExternalTriggerRecord>,
    ) -> Self {
        crate::diagnostics::record_runtime_projection_cache_rebuild();
        let task_agent_id = agent_id.clone();
        let work_item_agent_id = agent_id.clone();
        let timer_agent_id = agent_id.clone();
        let external_trigger_agent_id = agent_id.clone();
        Self {
            agent_id,
            tasks: latest_by(
                tasks
                    .into_iter()
                    .filter(|record| record.agent_id == task_agent_id),
                |record| record.id.clone(),
            ),
            work_items: latest_by(
                work_items
                    .into_iter()
                    .filter(|record| record.agent_id == work_item_agent_id),
                |record| record.id.clone(),
            ),
            timers: latest_by(
                timers
                    .into_iter()
                    .filter(|record| record.agent_id == timer_agent_id),
                |record| record.id.clone(),
            ),
            external_triggers: latest_by(
                external_triggers
                    .into_iter()
                    .filter(|record| record.target_agent_id == external_trigger_agent_id),
                |record| record.external_trigger_id.clone(),
            ),
        }
    }

    fn upsert_task(&mut self, record: TaskRecord) {
        if record.agent_id == self.agent_id {
            self.tasks.insert(record.id.clone(), record);
        }
    }

    fn upsert_work_item(&mut self, record: crate::types::WorkItemRecord) {
        if record.agent_id == self.agent_id {
            self.work_items.insert(record.id.clone(), record);
        }
    }

    fn upsert_timer(&mut self, record: TimerRecord) {
        if record.agent_id == self.agent_id {
            self.timers.insert(record.id.clone(), record);
        }
    }

    fn upsert_external_trigger(&mut self, record: ExternalTriggerRecord) {
        if record.target_agent_id == self.agent_id {
            self.external_triggers
                .insert(record.external_trigger_id.clone(), record);
        }
    }

    fn active_tasks(&self, limit: usize) -> Vec<TaskRecord> {
        let mut records = self
            .tasks
            .values()
            .filter(|record| {
                matches!(
                    record.status,
                    TaskStatus::Queued | TaskStatus::Running | TaskStatus::Cancelling
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn latest_tasks(&self, limit: usize) -> Vec<TaskRecord> {
        let mut records = self.tasks.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn latest_work_items(&self, limit: usize) -> Vec<crate::types::WorkItemRecord> {
        let mut records = self.work_items.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| right.created_at.cmp(&left.created_at))
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn recent_timers(&self, limit: usize) -> Vec<TimerRecord> {
        let mut records = self.timers.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| {
            right
                .created_at
                .cmp(&left.created_at)
                .then_with(|| left.id.cmp(&right.id))
        });
        take_limit(records, limit)
    }

    fn latest_external_triggers(&self) -> Vec<ExternalTriggerRecord> {
        let mut records = self.external_triggers.values().cloned().collect::<Vec<_>>();
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        records
    }
}

fn latest_by<T, F>(records: impl IntoIterator<Item = T>, key: F) -> HashMap<String, T>
where
    F: Fn(&T) -> String,
{
    let mut latest = HashMap::new();
    for record in records {
        latest.insert(key(&record), record);
    }
    latest
}

fn take_limit<T>(mut records: Vec<T>, limit: usize) -> Vec<T> {
    if records.len() > limit {
        records.truncate(limit);
    }
    records
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct ViewImageObservationCacheKey {
    pub(crate) visual_reference_id: String,
    pub(crate) prompt: String,
    pub(crate) observation_schema: String,
    pub(crate) generation_policy: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BuiltinWebSearchProbeKey {
    provider_id: String,
    provider_model_ref: String,
    provider_transport: String,
    provider_base_url: String,
    advertised_tool_type: String,
    backend_kind: String,
}

impl BuiltinWebSearchProbeKey {
    fn from_capability(capability: &ProviderBuiltinWebSearchCapability) -> Self {
        Self {
            provider_id: capability.provider_id.clone(),
            provider_model_ref: capability.provider_model_ref.clone(),
            provider_transport: capability.provider_transport.clone(),
            provider_base_url: capability.provider_base_url.clone(),
            advertised_tool_type: capability.advertised_tool_type.clone(),
            backend_kind: capability.backend_kind.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinWebSearchProbeCacheEntry {
    status: BuiltinWebSearchProbeStatus,
    reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[allow(dead_code)]
#[serde(rename_all = "snake_case")]
enum BuiltinWebSearchProbeStatus {
    Supported,
    Unsupported,
    TransientFailure,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum BuiltinWebSearchSelectionStatus {
    Selected,
    Disabled,
    Unsupported,
    NotDeclared,
    NotRequested,
    TransientProbeFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BuiltinWebSearchSelectionDiagnostics {
    status: BuiltinWebSearchSelectionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_model_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_transport: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_base_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    advertised_tool_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend_kind: Option<String>,
    probe_status: BuiltinWebSearchProbeStatus,
    probe_cache_hit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuiltinWebSearchSelection {
    request: Option<ProviderNativeWebSearchRequest>,
    diagnostics: BuiltinWebSearchSelectionDiagnostics,
}

#[derive(Debug)]
struct RuntimeAgent {
    state: AgentState,
    last_persisted_state: AgentState,
    queue: RuntimeQueue,
    current_run_abort: Option<CurrentRunAbortHandle>,
}

impl RuntimeAgent {
    fn persist_state(&mut self, storage: &AppStorage) -> Result<()> {
        let started = std::time::Instant::now();
        if let Err(error) = storage.write_agent(&self.state) {
            self.state = self.last_persisted_state.clone();
            crate::diagnostics::record_storage_persist_state(started.elapsed());
            return Err(error);
        }
        self.last_persisted_state = self.state.clone();
        crate::diagnostics::record_storage_persist_state(started.elapsed());
        Ok(())
    }
}

#[derive(Debug, Clone)]
struct CurrentRunAbortHandle {
    run_id: String,
    token: CancellationToken,
    reason: Arc<StdMutex<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CurrentRunAbortMode {
    StopAfterAbort,
}

impl CurrentRunAbortMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StopAfterAbort => "stop_after_abort",
        }
    }
}

impl Default for CurrentRunAbortMode {
    fn default() -> Self {
        Self::StopAfterAbort
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRunAbortRequest {
    pub run_id: Option<String>,
    pub mode: CurrentRunAbortMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentRunAbortOutcome {
    pub agent_id: String,
    pub run_id: String,
    pub mode: CurrentRunAbortMode,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CurrentRunAbortError {
    #[error("agent {agent_id} has no current run to abort")]
    NoCurrentRun { agent_id: String },
    #[error("stale run_id {requested_run_id}; current run is {current_run_id}")]
    StaleRunId {
        requested_run_id: String,
        current_run_id: String,
    },
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("current run aborted: {reason}")]
pub struct CurrentRunAborted {
    pub run_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CurrentRunAbortSnapshot {
    pub(crate) run_id: String,
    pub(crate) token: CancellationToken,
    pub(crate) reason: Arc<StdMutex<String>>,
}

impl CurrentRunAbortSnapshot {
    pub(crate) fn reason(&self) -> String {
        self.reason
            .lock()
            .map(|reason| reason.clone())
            .unwrap_or_else(|_| "operator_aborted".into())
    }
}

impl RuntimeHandle {
    pub(super) fn now(&self) -> chrono::DateTime<chrono::Utc> {
        self.inner.clock.now()
    }

    fn scheduler_protocol_production_commands_enabled(&self) -> bool {
        self.inner
            .scheduler_protocol_production_commands_enabled
            .load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn set_scheduler_protocol_production_commands_enabled(&self, enabled: bool) {
        self.inner
            .scheduler_protocol_production_commands_enabled
            .store(enabled, Ordering::SeqCst);
    }

    fn take_transition_fault(&self) -> Option<TransitionFaultPoint> {
        #[cfg(test)]
        {
            return self
                .inner
                .transition_faults
                .lock()
                .expect("transition fault plan lock poisoned")
                .pop_front();
        }
        #[cfg(not(test))]
        {
            None
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_next_transition_fault(&self, fault: TransitionFaultPoint) {
        let mut faults = self
            .inner
            .transition_faults
            .lock()
            .expect("transition fault plan lock poisoned");
        assert!(
            faults.is_empty(),
            "a transition fault is already armed for this runtime fixture"
        );
        faults.push_back(fault);
    }

    #[cfg(test)]
    pub(crate) fn omit_next_scheduler_claim_shadow_comparison(&self) {
        assert!(
            !self
                .inner
                .omit_next_scheduler_claim_shadow_comparison
                .swap(true, Ordering::SeqCst),
            "scheduler claim shadow comparison omission is already armed"
        );
    }

    #[cfg(test)]
    pub(crate) fn inject_runtime_loop_failure_after_next_claim(&self) {
        self.inner
            .fail_after_next_runtime_claim
            .store(true, Ordering::SeqCst);
        self.inner.notify.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn take_transition_warnings(&self) -> Vec<PostCommitWarning> {
        std::mem::take(
            &mut *self
                .inner
                .transition_warnings
                .lock()
                .expect("transition warning lock poisoned"),
        )
    }

    pub(crate) async fn apply_transition_commit(
        &self,
        commit: TransitionCommit,
    ) -> TransitionApplyResult {
        if !commit.applied {
            return TransitionApplyResult::default();
        }
        let effects = commit.effects;
        let mut warnings = Vec::new();
        if effects.fault == Some(TransitionFaultPoint::BeforeCacheUpdate) {
            warnings.push(PostCommitWarning {
                effect: "projection_cache_update",
                message: "injected runtime transition post-commit fault".into(),
            });
        } else {
            let mut cache = self.inner.projection_cache.lock().await;
            for record in &effects.work_items {
                cache.upsert_work_item(record.clone());
            }
            for record in &effects.tasks {
                cache.upsert_task(record.clone());
            }
        }
        if let Some(mutation) = effects.agent_state.as_ref() {
            let mut guard = self.inner.agent.lock().await;
            if mutation
                .expected
                .as_ref()
                .is_none_or(|expected| guard.state == **expected)
            {
                guard.state = mutation.record.as_ref().clone();
                guard.last_persisted_state = mutation.record.as_ref().clone();
            } else {
                warnings.push(PostCommitWarning {
                    effect: "agent_state_projection_update",
                    message: "agent state changed after transition commit; retained newer in-memory state"
                        .into(),
                });
            }
        }
        if effects.fault == Some(TransitionFaultPoint::BeforeEventPublication) {
            warnings.push(PostCommitWarning {
                effect: "event_publication",
                message: "injected runtime transition post-commit fault".into(),
            });
        } else {
            warnings.extend(self.inner.storage.publish_transition_events(&effects));
        }
        warnings.extend(self.inner.storage.notify_transition_memory_index(&effects));
        if effects.notify_scheduler {
            if effects.fault == Some(TransitionFaultPoint::BeforeSchedulerNotification) {
                warnings.push(PostCommitWarning {
                    effect: "scheduler_notification",
                    message: "injected runtime transition post-commit fault".into(),
                });
            } else {
                self.inner.notify.notify_one();
            }
        }
        let result = TransitionApplyResult {
            applied: true,
            warnings,
        };
        for warning in &result.warnings {
            tracing::warn!(
                effect = warning.effect,
                error = %warning.message,
                "runtime transition committed with post-commit warning"
            );
        }
        #[cfg(test)]
        self.inner
            .transition_warnings
            .lock()
            .expect("transition warning lock poisoned")
            .extend(result.warnings.iter().cloned());
        result
    }

    pub(crate) async fn record_timer_projection(&self, record: &TimerRecord) -> Result<()> {
        self.inner.storage.append_timer(record)?;
        self.inner
            .projection_cache
            .lock()
            .await
            .upsert_timer(record.clone());
        Ok(())
    }

    pub(crate) async fn cache_external_trigger_projection(&self, record: &ExternalTriggerRecord) {
        self.inner
            .projection_cache
            .lock()
            .await
            .upsert_external_trigger(record.clone());
    }

    pub(crate) fn work_item_written_event(
        &self,
        action: &str,
        record: &crate::types::WorkItemRecord,
        extra: Value,
    ) -> AuditEvent {
        let payload = WorkItemLifecycleAuditEvent::from_work_item(action, record);
        let mut event = AuditEvent::typed(RuntimeEventKind::WorkItemWritten, &payload)
            .expect("work item lifecycle payload must serialize");
        if let (Some(payload), Some(extra)) = (event.data.as_object_mut(), extra.as_object()) {
            for (key, value) in extra {
                payload.insert(key.clone(), value.clone());
            }
        }
        event
    }

    pub(crate) fn work_item_plan_artifact_refreshed_event(
        &self,
        record: &crate::types::WorkItemRecord,
    ) -> Option<AuditEvent> {
        let Some(artifact) = record.plan_artifact.as_ref() else {
            return None;
        };
        Some(AuditEvent::legacy(
            "work_item_plan_artifact_refreshed",
            serde_json::json!({
                "work_item_id": record.id,
                "revision": record.revision,
                "plan_artifact_path": artifact.path,
                "plan_artifact_hash": artifact.hash,
                "plan_artifact_bytes": artifact.bytes,
                "plan_artifact_updated_at": artifact.updated_at,
                "preview_complete": artifact.preview_complete,
            }),
        ))
    }
}

impl RuntimeHandle {
    pub(crate) async fn update_agent_state<F>(&self, mutate: F) -> Result<AgentState>
    where
        F: FnOnce(&mut AgentState) -> Result<()>,
    {
        let mut guard = self.inner.agent.lock().await;
        mutate(&mut guard.state)?;
        guard.persist_state(&self.inner.storage)?;
        Ok(guard.state.clone())
    }

    fn build_execution_root_id(
        workspace_id: &str,
        projection_kind: WorkspaceProjectionKind,
        execution_root: &Path,
    ) -> Result<String> {
        workspace::build_execution_root_id(workspace_id, projection_kind, execution_root)
    }

    fn agent_home_workspace_entry(data_dir: &Path, agent_id: &str) -> crate::types::WorkspaceEntry {
        workspace::agent_home_workspace_entry(data_dir, agent_id)
    }

    pub fn storage(&self) -> &AppStorage {
        &self.inner.storage
    }

    #[cfg(test)]
    pub(crate) fn runtime_db(&self) -> &crate::runtime_db::RuntimeDb {
        &self.inner.runtime_db
    }

    pub fn object_query_cache(&self) -> Arc<crate::object_query_cache::ObjectQueryCache> {
        self.inner.object_query_cache.clone()
    }

    pub fn poll_activity_marker(&self) -> Result<PollActivityMarker> {
        self.inner.storage.poll_activity_marker()
    }

    pub async fn abort_current_run(
        &self,
        request: CurrentRunAbortRequest,
    ) -> Result<CurrentRunAbortOutcome> {
        let mut guard = self.inner.agent.lock().await;
        let agent_id = guard.state.id.clone();
        let Some(handle) = guard.current_run_abort.as_ref().cloned() else {
            return Err(CurrentRunAbortError::NoCurrentRun { agent_id }.into());
        };
        if let Some(expected_run_id) = request.run_id.as_deref() {
            if expected_run_id != handle.run_id {
                return Err(CurrentRunAbortError::StaleRunId {
                    requested_run_id: expected_run_id.to_string(),
                    current_run_id: handle.run_id.clone(),
                }
                .into());
            }
        }

        if let Ok(mut reason) = handle.reason.lock() {
            *reason = "operator_aborted".into();
        }
        handle.token.cancel();
        scheduler::apply_stop_projection(&mut guard.state);
        guard.persist_state(&self.inner.storage)?;
        drop(guard);

        self.inner.storage.append_event(&AuditEvent::legacy(
            "current_run_aborted",
            serde_json::json!({
                "agent_id": agent_id,
                "run_id": handle.run_id,
                "mode": request.mode.as_str(),
                "reason": "operator_aborted",
            }),
        ))?;
        self.inner.notify.notify_waiters();
        Ok(CurrentRunAbortOutcome {
            agent_id,
            run_id: handle.run_id,
            mode: request.mode,
        })
    }

    pub(crate) async fn current_run_abort_token(&self) -> Option<CurrentRunAbortSnapshot> {
        let guard = self.inner.agent.lock().await;
        guard
            .current_run_abort
            .as_ref()
            .map(|handle| CurrentRunAbortSnapshot {
                run_id: handle.run_id.clone(),
                token: handle.token.clone(),
                reason: handle.reason.clone(),
            })
    }

    pub fn all_events(&self) -> Result<Vec<AuditEvent>> {
        self.inner.storage.read_recent_events(usize::MAX)
    }

    pub fn all_messages(&self) -> Result<Vec<MessageEnvelope>> {
        self.inner.storage.read_all_messages()
    }

    pub fn all_tool_executions(&self) -> Result<Vec<ToolExecutionRecord>> {
        self.inner.storage.read_recent_tool_executions(usize::MAX)
    }

    pub async fn latest_task_records_snapshot(&self) -> Result<Vec<TaskRecord>> {
        let mut tasks_by_id = self
            .inner
            .storage
            .latest_task_records()?
            .into_iter()
            .map(|task| (task.id.clone(), task))
            .collect::<HashMap<_, _>>();
        for task in self
            .inner
            .projection_cache
            .lock()
            .await
            .latest_tasks(usize::MAX)
        {
            match tasks_by_id.entry(task.id.clone()) {
                Entry::Occupied(mut entry) => {
                    if task_state_reducer::should_ignore_task_update(
                        Some(entry.get().clone()),
                        &task,
                    ) {
                        continue;
                    }
                    entry.insert(task);
                }
                Entry::Vacant(entry) => {
                    entry.insert(task);
                }
            }
        }
        let mut tasks = tasks_by_id.into_values().collect::<Vec<_>>();
        tasks.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
        Ok(tasks)
    }

    pub(crate) fn agent_home(&self) -> PathBuf {
        self.inner.storage.data_dir().to_path_buf()
    }

    pub fn workspace_root(&self) -> PathBuf {
        self.execution_root_sync()
    }

    pub(crate) fn system(&self) -> Arc<LocalSystem> {
        self.inner.system.clone()
    }

    pub(crate) fn web_config(&self) -> WebConfig {
        self.inner.config_snapshot.load().web_config.clone()
    }

    pub(crate) fn x_search_config(&self) -> Option<crate::config::XSearchRuntimeConfig> {
        self.inner.config_snapshot.load().x_search_config.clone()
    }

    fn user_home(&self) -> Option<PathBuf> {
        if let Some(provider_reconfig) =
            self.inner.config_snapshot.load().provider_reconfig.as_ref()
        {
            return Some(provider_reconfig.config.home_dir.clone());
        }
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn fallback_identity_view(&self, agent_id: &str) -> AgentIdentityView {
        let kind = if agent_id == self.inner.default_agent_id {
            AgentKind::Default
        } else {
            AgentKind::Named
        };
        AgentIdentityView {
            agent_id: agent_id.to_string(),
            kind,
            visibility: crate::types::AgentVisibility::Public,
            ownership: crate::types::AgentOwnership::SelfOwned,
            profile_preset: crate::types::AgentProfilePreset::PublicNamed,
            status: crate::types::AgentRegistryStatus::Active,
            is_default_agent: agent_id == self.inner.default_agent_id,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        }
    }

    pub(crate) async fn agent_identity_view(&self) -> Result<AgentIdentityView> {
        let agent_id = self.agent_id().await?;
        if let Some(bridge) = self.inner.host_bridge.as_ref() {
            if let Some(identity) = bridge.identity_for_agent(&agent_id).await? {
                return Ok(AgentIdentityView::from_record(
                    &identity,
                    &self.inner.default_agent_id,
                ));
            }
        }
        Ok(self.fallback_identity_view(&agent_id))
    }

    fn skill_visibility(&self, identity: &AgentIdentityView) -> SkillVisibility {
        if identity.kind == AgentKind::Default {
            SkillVisibility::DefaultAgent
        } else {
            SkillVisibility::NonDefaultAgent
        }
    }

    pub(crate) async fn inherit_from_parent_state(&self, parent_state: &AgentState) -> Result<()> {
        let next_state = {
            let guard = self.inner.agent.lock().await;
            let mut next_state = guard.state.clone();
            next_state.attached_workspaces =
                workspace::inherited_attached_workspaces_for_agent(parent_state, &next_state.id);
            next_state.active_workspace_entry = parent_state.active_workspace_entry.clone();
            next_state.worktree_session = parent_state.worktree_session.clone();
            workspace::canonicalize_agent_home_bindings(
                &mut next_state,
                self.inner.storage.data_dir(),
                &guard.state.id,
            )?;
            if next_state
                .active_workspace_entry
                .as_ref()
                .is_some_and(|entry| {
                    entry.workspace_id == AGENT_HOME_WORKSPACE_ID
                        || entry.workspace_id.starts_with("agent_home:")
                })
            {
                let access_mode = next_state
                    .active_workspace_entry
                    .as_ref()
                    .map(|entry| entry.access_mode)
                    .unwrap_or(WorkspaceAccessMode::ExclusiveWrite);
                next_state.active_workspace_entry =
                    Some(workspace::canonical_agent_home_active_entry(
                        self.inner.storage.data_dir(),
                        &guard.state.id,
                        access_mode,
                    )?);
                next_state.worktree_session = None;
            }
            next_state.execution_profile = parent_state.execution_profile.clone();
            next_state.model_override = parent_state.model_override.clone();
            next_state
        };
        if self
            .inner
            .config_snapshot
            .load()
            .provider_reconfig
            .is_some()
        {
            self.reconfigure_provider_for_state(&next_state).await?;
        }
        self.update_agent_state(|state| {
            *state = next_state;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn inherit_attached_workspaces_from_parent_state(
        &self,
        parent_state: &AgentState,
    ) -> Result<()> {
        let next_state = {
            let guard = self.inner.agent.lock().await;
            let mut next_state = guard.state.clone();
            next_state.attached_workspaces =
                workspace::inherited_attached_workspaces_for_agent(parent_state, &next_state.id);
            next_state.active_workspace_entry = None;
            next_state.worktree_session = None;
            workspace::canonicalize_agent_home_bindings(
                &mut next_state,
                self.inner.storage.data_dir(),
                &guard.state.id,
            )?;
            next_state.execution_profile = parent_state.execution_profile.clone();
            next_state.model_override = parent_state.model_override.clone();
            next_state
        };
        if self
            .inner
            .config_snapshot
            .load()
            .provider_reconfig
            .is_some()
        {
            self.reconfigure_provider_for_state(&next_state).await?;
        }
        self.update_agent_state(|state| {
            *state = next_state;
            Ok(())
        })
        .await?;
        Ok(())
    }

    pub(crate) async fn workspace_view(&self) -> Result<WorkspaceView> {
        let guard = self.inner.agent.lock().await;
        self.workspace_view_from_state(&guard.state)
    }

    pub(crate) fn workspace_view_for_root(
        &self,
        execution_root: PathBuf,
        cwd: PathBuf,
        worktree_root: Option<PathBuf>,
    ) -> Result<WorkspaceView> {
        workspace::workspace_view_for_root(&self.inner.storage, execution_root, cwd, worktree_root)
    }

    fn workspace_view_from_state(&self, state: &AgentState) -> Result<WorkspaceView> {
        workspace::workspace_view_from_state(state, self.inner.storage.data_dir().to_path_buf())
    }

    fn execution_snapshot_for_view(
        &self,
        profile: crate::system::ExecutionProfile,
        workspace: &WorkspaceView,
        attached_workspace_ids: &[String],
    ) -> ExecutionSnapshot {
        let mut snapshot = workspace::execution_snapshot_for_view(
            profile,
            workspace,
            attached_workspace_ids,
            &self.inner.storage,
        );
        // Populate execution_roots from the runtime DB registry for all
        // attached workspaces, so the provider turn resolver can resolve
        // `?root=` parameters in workspace:// URIs.
        let repo = self.inner.runtime_db.execution_root_entries();
        let mut roots = Vec::new();
        for ws_id in attached_workspace_ids {
            if let Ok(entries) = repo.active_for_workspace(ws_id) {
                for entry in entries {
                    roots.push(crate::system::ExecutionRootRef {
                        execution_root_id: entry.execution_root_id,
                        workspace_id: entry.workspace_id,
                        filesystem_path: entry.filesystem_path,
                    });
                }
            }
        }
        snapshot.execution_roots = roots;
        snapshot
    }

    fn workspace_anchor_for_state_ref<'a>(&self, state: &'a AgentState) -> Option<&'a Path> {
        workspace::workspace_anchor_for_state_ref(state)
    }

    fn execution_root_sync(&self) -> PathBuf {
        workspace::execution_root_sync(&self.inner.storage)
    }

    pub(crate) async fn effective_execution(
        &self,
        scope: ExecutionScopeKind,
    ) -> Result<EffectiveExecution> {
        let guard = self.inner.agent.lock().await;
        let profile = guard.state.execution_profile.clone();
        let attached_workspace_ids = guard.state.attached_workspaces.clone();
        drop(guard);
        let workspace = self.workspace_view().await?;
        Ok(workspace::build_effective_execution(
            &self.inner.storage,
            scope,
            profile,
            workspace,
            &attached_workspace_ids,
        ))
    }

    pub(crate) async fn effective_execution_for_workspace(
        &self,
        scope: ExecutionScopeKind,
        workspace: WorkspaceView,
    ) -> Result<EffectiveExecution> {
        let guard = self.inner.agent.lock().await;
        let profile = guard.state.execution_profile.clone();
        let attached_workspace_ids = guard.state.attached_workspaces.clone();
        drop(guard);
        Ok(workspace::build_effective_execution(
            &self.inner.storage,
            scope,
            profile,
            workspace,
            &attached_workspace_ids,
        ))
    }

    pub async fn execution_snapshot(&self) -> Result<ExecutionSnapshot> {
        Ok(self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?
            .snapshot())
    }

    pub(crate) async fn loaded_agents_md(&self) -> Result<LoadedAgentsMd> {
        let guard = self.inner.agent.lock().await;
        self.loaded_agents_md_for_state(&guard.state)
    }

    fn loaded_agents_md_for_state(&self, state: &AgentState) -> Result<LoadedAgentsMd> {
        load_agents_md(
            self.user_home().as_deref(),
            self.agent_home().as_path(),
            self.workspace_anchor_for_state_ref(state),
        )
    }

    fn loaded_agent_memory_for_state(&self) -> Result<LoadedAgentMemory> {
        load_agent_memory(self.agent_home().as_path())
    }

    pub(crate) async fn skills_runtime_view(
        &self,
        identity: &AgentIdentityView,
    ) -> Result<SkillsRuntimeView> {
        let guard = self.inner.agent.lock().await;
        self.skills_runtime_view_for_state(&guard.state, identity)
            .await
    }

    async fn skills_runtime_view_for_state(
        &self,
        state: &AgentState,
        identity: &AgentIdentityView,
    ) -> Result<SkillsRuntimeView> {
        let skill_roots = effective_skill_root_registrations(
            self.skill_visibility(identity),
            self.user_home().as_deref(),
            &state.id,
            self.agent_home().as_path(),
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_anchor.as_path()),
        );
        let mut view = if let Some(bridge) = self.inner.host_bridge.as_ref() {
            let registry = bridge.skills_registry()?;
            let mut registry = registry.write().await;
            registry.sync_effective_roots(skill_roots.clone())?;
            skills_runtime_view_from_catalog(
                registry.catalog_for_roots(&skill_roots, None),
                &skill_roots,
                &state.active_skills,
            )
        } else {
            let mut registry = crate::skills::SkillsRegistry::new();
            registry.replace_roots(skill_roots.clone())?;
            skills_runtime_view_from_catalog(registry.catalog(), &skill_roots, &state.active_skills)
        };
        view.agent_templates_catalog = discover_agent_templates_catalog(
            self.user_home().as_deref(),
            self.agent_home().as_path(),
        );
        Ok(view)
    }

    pub(crate) async fn sync_effective_skill_roots_for_state(
        &self,
        state: &AgentState,
    ) -> Result<()> {
        let Some(bridge) = self.inner.host_bridge.as_ref() else {
            return Ok(());
        };
        let identity = self.agent_identity_view().await?;
        let skill_roots = effective_skill_root_registrations(
            self.skill_visibility(&identity),
            self.user_home().as_deref(),
            &state.id,
            self.agent_home().as_path(),
            state
                .active_workspace_entry
                .as_ref()
                .map(|entry| entry.workspace_anchor.as_path()),
        );
        let registry = bridge.skills_registry()?;
        registry.write().await.sync_effective_roots(skill_roots)?;
        Ok(())
    }

    async fn begin_interactive_turn(
        &self,
        message: Option<&MessageEnvelope>,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        let state = {
            let mut guard = self.inner.agent.lock().await;
            guard.state.turn_index += 1;
            let turn_id = message
                .and_then(|message| normalized_turn_id(message.turn_id.as_deref()))
                .unwrap_or_else(crate::ids::turn_id);
            guard.state.current_turn_id = Some(turn_id.clone());
            guard.state.last_turn_terminal = None;
            if guard.state.current_turn_work_item_id.is_none() {
                guard.state.current_turn_work_item_id = guard.state.current_work_item_id.clone();
            }
            guard.state.current_execution_binding = message.map(|message| {
                let work_item_id = message
                    .work_item_id
                    .clone()
                    .or_else(|| guard.state.current_turn_work_item_id.clone());
                let claimed_work_revision = work_item_id
                    .as_deref()
                    .and_then(|work_item_id| {
                        self.inner
                            .runtime_db
                            .work_items()
                            .latest(work_item_id)
                            .ok()
                            .flatten()
                    })
                    .map(|work_item| work_item.revision);
                let activation_id = Some(scheduler_executor::canonical_activation_id(&message.id))
                    .filter(|activation_id| {
                        self.inner
                            .runtime_db
                            .transitions()
                            .load_scheduler_protocol_snapshot_if_initialized(&message.agent_id)
                            .ok()
                            .flatten()
                            .is_some_and(|snapshot| {
                                snapshot.activations.contains_key(activation_id)
                            })
                    });
                WorkItemExecutionBinding {
                    activation_id,
                    source_message_id: message.id.clone(),
                    turn_id,
                    work_item_id,
                    claimed_work_revision,
                }
            });
            guard.state.current_turn_operator_binding_id =
                operator_binding_id.and_then(|binding_id| {
                    let binding_id = binding_id.trim();
                    if binding_id.is_empty() {
                        None
                    } else {
                        Some(binding_id.to_string())
                    }
                });
            guard.state.current_turn_operator_reply_route_id =
                operator_reply_route_id.and_then(|route| {
                    let route = route.trim();
                    if route.is_empty() {
                        None
                    } else {
                        Some(route.to_string())
                    }
                });
            guard.state.active_skills.retain(|skill| {
                matches!(skill.activation_state, SkillActivationState::SessionActive)
            });
            guard.persist_state(&self.inner.storage)?;
            guard.state.clone()
        };
        self.append_state_changed_events(&state)?;
        if let Some(message) = message {
            self.inner.storage.append_event(&AuditEvent::legacy(
                "turn_started",
                serde_json::json!({
                    "agent_id": message.agent_id.clone(),
                    "message_id": message.id.clone(),
                    "turn_id": state.current_turn_id.clone(),
                    "message_kind": message.kind.clone(),
                    "run_id": state.current_run_id,
                    "turn_index": state.turn_index,
                }),
            ))?;
        }
        Ok(())
    }

    #[cfg(test)]
    async fn begin_interactive_turn_for_test(
        &self,
        operator_binding_id: Option<&str>,
        operator_reply_route_id: Option<&str>,
    ) -> Result<()> {
        self.begin_interactive_turn(None, operator_binding_id, operator_reply_route_id)
            .await
    }

    fn operator_transport_from_message(
        message: &MessageEnvelope,
    ) -> (Option<String>, Option<String>) {
        let transport = message
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("operator_transport"))
            .cloned();
        let binding_id = transport
            .as_ref()
            .and_then(|metadata| metadata.get("binding_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|binding_id| !binding_id.is_empty())
            .map(ToString::to_string);
        let reply_route_id = transport
            .as_ref()
            .and_then(|metadata| metadata.get("reply_route_id"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|route| !route.is_empty())
            .map(ToString::to_string);
        (binding_id, reply_route_id)
    }

    pub(crate) async fn promote_turn_active_skills(&self) -> Result<()> {
        let mut guard = self.inner.agent.lock().await;
        for skill in &mut guard.state.active_skills {
            if matches!(skill.activation_state, SkillActivationState::TurnActive) {
                skill.activation_state = SkillActivationState::SessionActive;
            }
        }
        guard.persist_state(&self.inner.storage)?;
        Ok(())
    }

    pub(crate) async fn record_skill_tool_activation(
        &self,
        tool_name: &str,
        input: &serde_json::Value,
        result: &ToolResult,
    ) -> Result<()> {
        match tool_name {
            "Read" | "ReadFile" => {
                if let Some(file_path) = input.get("file_path").and_then(|value| value.as_str()) {
                    self.record_skill_read_activation(file_path, SkillLoadReason::ReadSkillMd)
                        .await?;
                }
            }
            crate::tool::names::EXEC_COMMAND => {
                if let Some(command) = input.get("cmd").and_then(|value| value.as_str()) {
                    self.record_skill_command_activation(command).await?;
                }
            }
            crate::tool::names::EXEC_COMMAND_BATCH => {
                if let Some(batch) = result
                    .envelope
                    .result
                    .as_ref()
                    .and_then(decode_exec_command_batch_result)
                {
                    for item in batch.items {
                        if matches!(item.status, ExecCommandBatchItemStatus::Completed) {
                            self.record_skill_command_activation(&item.cmd).await?;
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) async fn record_skill_read_activation(
        &self,
        file_path: &str,
        load_reason: SkillLoadReason,
    ) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let resolved_path = execution.workspace.resolve_read_path(file_path)?;
        let state_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let identity = self.agent_identity_view().await?;
        let skills = self
            .skills_runtime_view_for_state(&state_snapshot, &identity)
            .await?;
        let Some(skill) = skill_for_activation_path(&skills.discoverable_skills, &resolved_path)
        else {
            return Ok(());
        };
        let mut guard = self.inner.agent.lock().await;
        let turn_index = guard.state.turn_index;
        let agent_id = guard.state.id.clone();
        let run_id = guard.state.current_run_id.clone();

        let repeated = if let Some(existing) = guard
            .state
            .active_skills
            .iter_mut()
            .find(|record| record.skill_id == skill.skill_id)
        {
            existing.activation_state = SkillActivationState::TurnActive;
            existing.activation_source = SkillActivationSource::ImplicitFromCatalog;
            existing.activated_at_turn = turn_index;
            true
        } else {
            guard
                .state
                .active_skills
                .push(crate::types::ActiveSkillRecord {
                    skill_id: skill.skill_id.clone(),
                    name: skill.name.clone(),
                    path: skill.path.clone(),
                    scope: skill.scope.clone(),
                    agent_id: agent_id.clone(),
                    activation_source: SkillActivationSource::ImplicitFromCatalog,
                    activation_state: SkillActivationState::TurnActive,
                    activated_at_turn: turn_index,
                });
            false
        };
        guard.persist_state(&self.inner.storage)?;
        self.inner.storage.append_event(&AuditEvent::legacy(
            "skill_activated",
            serde_json::json!({
                "agent_id": agent_id,
                "skill_id": skill.skill_id,
                "skill_name": skill.name,
                "path": resolved_path,
                "entrypoint_path": skill.path,
                "scope": skill.scope,
                "activation_source": SkillActivationSource::ImplicitFromCatalog,
                "activation_state": SkillActivationState::TurnActive,
                "load_reason": load_reason,
                "turn_index": turn_index,
                "run_id": run_id,
                "repeated": repeated,
            }),
        ))?;
        Ok(())
    }

    async fn record_skill_command_activation(&self, command: &str) -> Result<()> {
        let execution = self
            .effective_execution(ExecutionScopeKind::AgentTurn)
            .await?;
        let state_snapshot = {
            let guard = self.inner.agent.lock().await;
            guard.state.clone()
        };
        let identity = self.agent_identity_view().await?;
        let skills = self
            .skills_runtime_view_for_state(&state_snapshot, &identity)
            .await?;

        for skill in skills.discoverable_skills {
            if let Some((activation_path, load_reason)) =
                command_skill_activation(command, &skill, execution.workspace.workspace_anchor())
            {
                let activation_path = activation_path.to_string_lossy().into_owned();
                self.record_skill_read_activation(&activation_path, load_reason)
                    .await?;
            }
        }
        Ok(())
    }

    pub async fn enqueue(&self, mut message: MessageEnvelope) -> Result<MessageEnvelope> {
        message.normalize_admission_fields();
        message.turn_id = normalized_turn_id(message.turn_id.as_deref());
        if message.turn_id.is_none() {
            message.turn_id = Some(crate::ids::turn_id());
        }
        let message_is_new = self
            .inner
            .storage
            .read_message_by_id(&message.id)?
            .is_none();
        let mut audit_events = vec![
            AuditEvent::legacy(
                "message_admitted",
                serde_json::json!({
                    "message_id": message.id.clone(),
                    "agent_id": message.agent_id.clone(),
                    "kind": message.kind.clone(),
                    "origin": message.origin.clone(),
                    "authority_class": message.authority_class,
                    "delivery_surface": message.delivery_surface,
                    "admission_context": message.admission_context,
                    "trigger_kind": message.trigger_kind,
                    "work_item_id": message.work_item_id.clone(),
                    "task_id": message.task_id.clone(),
                    "source_refs": message.source_refs.clone(),
                    "correlation_id": message.correlation_id.clone(),
                    "causation_id": message.causation_id.clone(),
                }),
            ),
            AuditEvent::typed(
                RuntimeEventKind::MessageEnqueued,
                &MessageLifecycleAuditEvent::from_message(&message),
            )?,
        ];
        let mut commit = {
            let mut guard = self.inner.agent.lock().await;
            let expected_persisted_state = guard.last_persisted_state.clone();
            let mut committed_state = guard.state.clone();
            let previous_status = committed_state.status.clone();
            let previous_sleeping_until = committed_state.sleeping_until;
            committed_state.pending = guard.queue.len().saturating_add(1);
            committed_state.last_wake_reason = Some(format!("{:?}", message.kind));
            committed_state.total_message_count = self
                .inner
                .storage
                .count_messages()?
                .saturating_add(usize::from(message_is_new));
            if scheduler::apply_message_wake_projection(&mut committed_state) {
                audit_events.push(AuditEvent::legacy(
                    "scheduler_posture_decision",
                    serde_json::json!({
                        "boundary": "message_admission",
                        "reason": "message_admission_wake",
                        "previous_status": previous_status,
                        "next_status": committed_state.status,
                        "evidence": [
                            format!("message_id={}", message.id),
                            format!("message_kind={:?}", message.kind),
                            format!("previous_sleeping_until={previous_sleeping_until:?}"),
                        ],
                    }),
                ));
            }
            let commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: message.agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Admit,
                    mutation: crate::runtime_db::transitions::QueueMutation::Upsert(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: QueueEntryStatus::Queued,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                    ),
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands: Vec::new(),
                    scheduler_authority_scenarios: Vec::new(),
                    scheduler_rollout_expectations: Vec::new(),
                    agent_state: Some(crate::runtime_db::transitions::AgentStateMutation {
                        expected: Some(Box::new(expected_persisted_state)),
                        record: Box::new(committed_state.clone()),
                    }),
                    message_evidence: vec![message.clone()],
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events,
                    scheduler_shadow_comparison: None,
                    scheduler_delivery_shadow_comparison: None,
                    scheduler_semantic_shadow: None,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                    brief_evidence: Vec::new(),
                },
            )?;
            guard.queue.push(message.clone());
            guard.state = committed_state.clone();
            guard.last_persisted_state = committed_state;
            let mut commit = commit;
            commit.effects.agent_state = None;
            commit
        };
        commit.effects.notify_scheduler = true;
        self.apply_transition_commit(commit).await;
        Ok(message)
    }

    pub(crate) fn append_audit_event(&self, kind: &str, data: serde_json::Value) -> Result<()> {
        self.inner
            .storage
            .append_event(&AuditEvent::legacy(kind, data))
    }

    #[cfg(test)]
    pub(crate) async fn commit_queue_settlement(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
    ) -> Result<bool> {
        self.commit_queue_terminal_settlement(record, audit_events, notify_scheduler, None)
            .await
    }

    async fn commit_queue_terminal_settlement(
        &self,
        record: QueueEntryRecord,
        audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&turn::TurnTerminalTransition>,
    ) -> Result<bool> {
        self.commit_queue_terminal_settlement_with_evidence(
            record,
            audit_events,
            notify_scheduler,
            terminal_transition,
            None,
            Vec::new(),
            Vec::new(),
        )
        .await
    }

    async fn commit_queue_terminal_settlement_with_evidence(
        &self,
        record: QueueEntryRecord,
        mut audit_events: Vec<AuditEvent>,
        notify_scheduler: bool,
        terminal_transition: Option<&turn::TurnTerminalTransition>,
        committed_agent_state: Option<AgentState>,
        transcript_entries: Vec<TranscriptEntry>,
        brief_evidence: Vec<BriefRecord>,
    ) -> Result<bool> {
        let scheduler_protocol_commands = self.canonical_queue_settlement_commands(&record).await?;
        let scheduler_rollout_expectations = self
            .inner
            .runtime_db
            .transitions()
            .scheduler_rollout_expectations(
                &[scheduler::SETTLEMENT_SCENARIO, scheduler::DELIVERY_SCENARIO],
                self.scheduler_protocol_production_commands_enabled(),
            )?;
        let (projection_state, queue_len, agent_state) = {
            let guard = self.inner.agent.lock().await;
            let mut state = committed_agent_state.unwrap_or_else(|| guard.state.clone());
            let agent_state = if let Some(transition) = terminal_transition {
                state.current_turn_id = Some(transition.terminal.turn_id.clone());
                state.last_turn_terminal = Some(transition.terminal.clone());
                Some(crate::runtime_db::transitions::AgentStateMutation {
                    expected: Some(Box::new(guard.last_persisted_state.clone())),
                    record: Box::new(state.clone()),
                })
            } else {
                None
            };
            (state, guard.queue.len(), agent_state)
        };
        let shadow_comparison = {
            let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
                &self.inner.storage,
                &projection_state,
                queue_len,
                self.now(),
            )?;
            scheduler::shadow_comparison_for_settlement(&projection, &record)
                .map(scheduler_executor::scheduler_shadow_comparison_command)
                .transpose()?
        };
        let delivery_shadow_comparison = {
            let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
                &self.inner.storage,
                &projection_state,
                queue_len,
                self.now(),
            )?;
            scheduler::shadow_comparison_for_delivery(&projection, &record)
                .map(scheduler_executor::scheduler_shadow_comparison_command)
                .transpose()?
        };
        if let Some(transition) = terminal_transition {
            audit_events.push(AuditEvent::legacy(
                "turn_terminal",
                serde_json::to_value(&transition.terminal)?,
            ));
            audit_events.push(Self::turn_record_audit_event(&transition.turn_record));
        }
        let commit = self.inner.runtime_db.transitions().commit_queue(
            &crate::runtime_db::transitions::QueueTransitionCommand {
                agent_id: record.agent_id.clone(),
                operation: crate::runtime_db::transitions::QueueOperation::Settle,
                mutation: crate::runtime_db::transitions::QueueMutation::Upsert(record),
                scheduler_claim_work_item: None,
                scheduler_protocol_bootstrap: None,
                scheduler_protocol_commands,
                scheduler_authority_scenarios: vec![
                    scheduler::SETTLEMENT_SCENARIO,
                    scheduler::DELIVERY_SCENARIO,
                ],
                scheduler_rollout_expectations,
                agent_state,
                message_evidence: Vec::new(),
                transcript_entries,
                turn_record: terminal_transition.map(|transition| transition.turn_record.clone()),
                audit_events,
                scheduler_shadow_comparison: shadow_comparison,
                scheduler_delivery_shadow_comparison: delivery_shadow_comparison,
                scheduler_semantic_shadow: None,
                notify_scheduler,
                fault: self.take_transition_fault(),
                brief_evidence,
            },
        )?;
        Ok(self.apply_transition_commit(commit).await.applied)
    }

    async fn canonical_queue_settlement_commands(
        &self,
        record: &QueueEntryRecord,
    ) -> Result<Vec<crate::domain::scheduler_protocol::ProtocolCommand>> {
        canonical_queue_settlement_commands_from_facts(
            &self.inner.storage,
            &self.inner.runtime_db,
            record,
        )
    }

    pub(crate) fn persist_transcript_evidence(&self, entry: &TranscriptEntry) -> Result<()> {
        self.inner.storage.append_transcript_entry(entry)?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub(crate) fn persist_tool_execution_evidence(
        &self,
        record: &ToolExecutionRecord,
    ) -> Result<()> {
        self.inner.storage.append_tool_execution(record)?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub(crate) fn persist_brief_evidence(&self, brief: &BriefRecord) -> Result<()> {
        self.inner.storage.append_brief(brief)?;
        self.inner.notify.notify_one();
        Ok(())
    }

    pub async fn run(self) -> Result<()> {
        self.bootstrap_recovery().await?;
        scheduler_executor::SchedulerDecisionExecutor::new(&self)
            .bootstrap_recovered()
            .await?;
        self.recover_scheduler_bootstrap_claims().await?;
        self.record_scheduler_bootstrap_diagnostics().await?;

        loop {
            let poll = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                .poll()
                .await?;

            let scheduled = match poll {
                scheduler_executor::RunLoopPoll::Shutdown => return Ok(()),
                scheduler_executor::RunLoopPoll::Stopped(state, queue_len) => {
                    let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
                        &self.inner.storage,
                        &state,
                        queue_len,
                        self.now(),
                    )?;
                    let decision = scheduler::decide_next_action(
                        &projection,
                        scheduler::SchedulerBoundary::RunLoop,
                        scheduler::SchedulerInput::Idle,
                    );
                    scheduler::append_scheduler_decision(
                        &self.inner.storage,
                        &self.inner.default_agent_id,
                        &decision,
                    )?;
                    return Ok(());
                }
                scheduler_executor::RunLoopPoll::Message(scheduled) => scheduled,
                scheduler_executor::RunLoopPoll::Idle => {
                    if self.maybe_emit_pending_system_tick(None).await? {
                        continue;
                    }
                    let idle_snapshot = {
                        let guard = self.inner.agent.lock().await;
                        (guard.state.clone(), guard.queue.len())
                    };
                    let projection = scheduler::SchedulerProjection::from_state_with_queue_len_at(
                        &self.inner.storage,
                        &idle_snapshot.0,
                        idle_snapshot.1,
                        self.now(),
                    )?;
                    let decision = scheduler::decide_next_action(
                        &projection,
                        scheduler::SchedulerBoundary::RunLoopIdle,
                        scheduler::SchedulerInput::Idle,
                    );
                    if !matches!(
                        decision.kind,
                        scheduler::SchedulerDecisionKind::Sleep
                            | scheduler::SchedulerDecisionKind::StayIdle
                    ) {
                        scheduler::append_scheduler_decision(
                            &self.inner.storage,
                            &self.inner.default_agent_id,
                            &decision,
                        )?;
                    }
                    let next_recheck_at = self.next_blocked_work_item_recheck_at().await?;
                    let idle_state = scheduler_executor::SchedulerDecisionExecutor::new(&self)
                        .transition_run_loop_idle_to_sleep(next_recheck_at)
                        .await?;
                    if let Some(idle_state) = idle_state {
                        self.append_state_changed_events(&idle_state)?;
                    }
                    if let Some(next_recheck_at) = next_recheck_at {
                        if next_recheck_at > self.now() {
                            tokio::select! {
                                _ = self.inner.notify.notified() => {}
                                _ = self.inner.clock.sleep_until(next_recheck_at) => {}
                            }
                        }
                    } else {
                        self.inner.notify.notified().await;
                    }
                    continue;
                }
            };

            let message = scheduled.message.clone();
            #[cfg(test)]
            if self
                .inner
                .fail_after_next_runtime_claim
                .swap(false, Ordering::SeqCst)
            {
                return Err(anyhow!(
                    "injected agent runtime loop failure after queue claim"
                ));
            }
            self.append_state_changed_events(&scheduled.running_state)?;

            let terminal_transition = match self
                .process_message_with_plan_deferred(
                    scheduled.message,
                    scheduled.dispatch_plan,
                    &scheduled.scheduler_decision,
                )
                .await
            {
                Ok(transition) => transition,
                Err(err) => {
                    let aborted = err.downcast_ref::<CurrentRunAborted>().cloned();
                    let (terminal, queue_status, mut audit_events, failure_artifacts) =
                        if let Some(aborted) = aborted.as_ref() {
                            (
                                self.build_turn_aborted_record(&aborted.reason, None, 0)
                                    .await,
                                QueueEntryStatus::Interrupted,
                                vec![AuditEvent::legacy(
                                    "message_processing_aborted",
                                    serde_json::json!({
                                        "message_id": message.id.clone(),
                                        "message_kind": message.kind.clone(),
                                        "run_id": aborted.run_id.clone(),
                                        "reason": aborted.reason.clone(),
                                    }),
                                )],
                                None,
                            )
                        } else {
                            let descriptor = describe_runtime_error(&err);
                            let terminal = self
                                .build_turn_aborted_record("runtime_error", None, 0)
                                .await;
                            error!(
                                message_id = %message.id,
                                turn_id = %terminal.turn_id,
                                domain = ?descriptor.domain,
                                code = %descriptor.code,
                                retryable = descriptor.retryable,
                                error = %descriptor.operator_message,
                                "failed to process message"
                            );
                            let (queue_status, settlement_reason) =
                                runtime_error_queue_settlement(&message.kind, &err);
                            let artifacts = self
                                .build_runtime_failure_artifacts(&message, &err, &terminal)
                                .await?;
                            let terminal_turn_id = terminal.turn_id.clone();
                            (
                                terminal,
                                queue_status.clone(),
                                vec![
                                    AuditEvent::legacy(
                                        "runtime_error",
                                        serde_json::json!({
                                            "message_id": message.id.clone(),
                                            "turn_id": terminal_turn_id,
                                            "message_kind": message.kind.clone(),
                                            "domain": descriptor.domain,
                                            "code": descriptor.code,
                                            "retryable": descriptor.retryable,
                                            "error": descriptor.operator_message,
                                            "recovery_hint": descriptor.recovery_hint,
                                            "safe_context": descriptor.safe_context,
                                            "source_chain": descriptor.source_chain,
                                            "token_usage": provider_attempt_timeline(&err)
                                                .and_then(|timeline| timeline.aggregated_token_usage.clone()),
                                            "provider_attempt_timeline": provider_attempt_timeline(&err),
                                        }),
                                    ),
                                    AuditEvent::legacy(
                                        "queue_entry_settled",
                                        serde_json::json!({
                                            "message_id": message.id.clone(),
                                            "message_kind": message.kind.clone(),
                                            "status": queue_status,
                                            "reason": settlement_reason,
                                        }),
                                    ),
                                ],
                                Some(artifacts),
                            )
                        };
                    if let Some(aborted) = aborted.as_ref() {
                        audit_events.push(AuditEvent::legacy(
                            "turn_terminal_aborted",
                            serde_json::json!({
                                "run_id": aborted.run_id,
                                "reason": aborted.reason,
                                "turn_id": terminal.turn_id,
                                "turn_index": terminal.turn_index,
                                "kind": terminal.kind,
                                "completed_at": terminal.completed_at,
                                "duration_ms": terminal.duration_ms,
                            }),
                        ));
                    }
                    let mut turn_record = self.build_turn_record(&terminal).await?;
                    if let Some(artifacts) = failure_artifacts.as_ref() {
                        if !turn_record.produced_brief_ids.contains(&artifacts.brief.id) {
                            turn_record
                                .produced_brief_ids
                                .push(artifacts.brief.id.clone());
                        }
                    }
                    let terminal_transition = turn::TurnTerminalTransition {
                        terminal,
                        turn_record,
                    };
                    let committed_state = {
                        let guard = self.inner.agent.lock().await;
                        let mut state = guard.state.clone();
                        if !matches!(state.status, AgentStatus::Stopped) {
                            if state.pending_fallback_model.is_some() {
                                let has_fallback = provider_attempt_timeline(&err)
                                    .and_then(|timeline| {
                                        timeline.pending_fallback_model_ref.as_deref()
                                    })
                                    .is_some();
                                if !has_fallback {
                                    state.pending_fallback_model = None;
                                }
                            }
                            scheduler::apply_idle_projection(&mut state, &self.inner.storage)?;
                        }
                        if let Some(artifacts) = failure_artifacts.as_ref() {
                            state.last_runtime_failure = Some(artifacts.failure_summary.clone());
                        }
                        state
                    };
                    self.commit_queue_terminal_settlement_with_evidence(
                        QueueEntryRecord {
                            message_id: message.id.clone(),
                            agent_id: message.agent_id.clone(),
                            priority: message.priority.clone(),
                            status: queue_status,
                            created_at: message.created_at,
                            updated_at: Utc::now(),
                        },
                        audit_events,
                        true,
                        Some(&terminal_transition),
                        Some(committed_state),
                        failure_artifacts
                            .as_ref()
                            .map(|artifacts| vec![artifacts.transcript.clone()])
                            .unwrap_or_default(),
                        failure_artifacts
                            .as_ref()
                            .map(|artifacts| vec![artifacts.brief.clone()])
                            .unwrap_or_default(),
                    )
                    .await?;
                    let failed_state = {
                        let mut guard = self.inner.agent.lock().await;
                        guard.current_run_abort = None;
                        guard.state.clone()
                    };
                    self.append_state_changed_events(&failed_state)?;
                    self.maybe_commit_turn_end_work_item_transition().await?;
                    self.record_closure_decision_event(Some(true)).await?;
                    self.maybe_emit_pending_system_tick(None).await?;
                    continue;
                }
            };
            {
                let processed_state = {
                    let mut guard = self.inner.agent.lock().await;
                    guard.current_run_abort = None;
                    guard.state.clone()
                };
                self.append_state_changed_events(&processed_state)?;
                self.commit_queue_terminal_settlement(
                    QueueEntryRecord {
                        message_id: message.id.clone(),
                        agent_id: message.agent_id.clone(),
                        priority: message.priority.clone(),
                        status: QueueEntryStatus::Processed,
                        created_at: message.created_at,
                        updated_at: Utc::now(),
                    },
                    vec![AuditEvent::legacy(
                        "queue_entry_settled",
                        serde_json::json!({
                            "message_id": message.id,
                            "message_kind": message.kind,
                            "status": QueueEntryStatus::Processed,
                        }),
                    )],
                    true,
                    terminal_transition.as_ref(),
                )
                .await?;
            }
        }
    }

    pub(crate) async fn record_runtime_loop_failure(&self, error: &anyhow::Error) {
        let descriptor = describe_runtime_error(error);
        let summary =
            Self::summarize_runtime_failure_error(&anyhow!(descriptor.operator_message.clone()));
        let occurred_at = Utc::now();
        let agent_id = self.inner.agent.lock().await.state.id.clone();
        let released_claims = match self.release_claimed_messages_for_runtime_restart().await {
            Ok(released) => released,
            Err(release_error) => {
                tracing::error!(
                    agent_id = %agent_id,
                    error = %release_error,
                    "failed to release claimed messages after runtime loop failure"
                );
                0
            }
        };
        tracing::error!(
            agent_id = %agent_id,
            domain = ?descriptor.domain,
            code = %descriptor.code,
            retryable = descriptor.retryable,
            error = %error,
            "agent runtime loop failed"
        );
        if let Err(persist_error) = self.inner.storage.append_event(&AuditEvent::legacy(
            "agent_runtime_loop_failed",
            serde_json::json!({
                "agent_id": agent_id,
                "error": summary,
                "domain": descriptor.domain,
                "code": descriptor.code,
                "retryable": descriptor.retryable,
                "recovery_hint": descriptor.recovery_hint,
                "safe_context": descriptor.safe_context,
                "source_chain": descriptor.source_chain,
                "recovery": "bounded_restart",
                "released_claims": released_claims,
            }),
        )) {
            tracing::error!(
                agent_id = %agent_id,
                error = %persist_error,
                "failed to persist agent runtime loop failure audit event"
            );
        }
        let mut guard = self.inner.agent.lock().await;
        guard.state.current_run_id = None;
        guard.current_run_abort = None;
        guard.state.last_runtime_failure = Some(RuntimeFailureSummary {
            occurred_at,
            summary,
            phase: RuntimeFailurePhase::RuntimeTurn,
            detail_hint: Some("the next host access will rebuild the runtime loop".into()),
            failure_artifact: None,
        });
        if let Err(persist_error) = guard.persist_state(&self.inner.storage) {
            tracing::error!(
                agent_id = %agent_id,
                error = %persist_error,
                "failed to persist agent runtime loop failure state"
            );
        }
    }

    async fn release_claimed_messages_for_runtime_restart(&self) -> Result<usize> {
        let agent_id = self.inner.agent.lock().await.state.id.clone();
        let claimed = self
            .inner
            .runtime_db
            .queue_entries()
            .recent(Some(&agent_id), usize::MAX)?
            .into_iter()
            .filter(|entry| entry.status == QueueEntryStatus::Dequeued)
            .collect::<Vec<_>>();
        let mut released = 0;
        for mut entry in claimed {
            entry.status = QueueEntryStatus::Interrupted;
            entry.updated_at = Utc::now();
            let message_id = entry.message_id.clone();
            let commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Release,
                    mutation: crate::runtime_db::transitions::QueueMutation::Upsert(entry),
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands: Vec::new(),
                    scheduler_authority_scenarios: Vec::new(),
                    scheduler_rollout_expectations: Vec::new(),
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: None,
                    audit_events: vec![AuditEvent::legacy(
                        "queue_claim_released_for_runtime_restart",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "message_id": message_id,
                            "status": QueueEntryStatus::Interrupted,
                        }),
                    )],
                    scheduler_shadow_comparison: None,
                    scheduler_delivery_shadow_comparison: None,
                    scheduler_semantic_shadow: None,
                    notify_scheduler: true,
                    fault: None,
                    brief_evidence: Vec::new(),
                },
            )?;
            if commit.applied {
                released += 1;
            }
            self.apply_transition_commit(commit).await;
        }
        Ok(released)
    }

    async fn recover_scheduler_bootstrap_claims(&self) -> Result<usize> {
        let scheduler_rollout_expectations = self
            .inner
            .runtime_db
            .transitions()
            .scheduler_rollout_expectations(
                &[scheduler::SETTLEMENT_SCENARIO],
                self.scheduler_protocol_production_commands_enabled(),
            )?;
        if scheduler_rollout_expectations.iter().any(|expectation| {
            expectation.mode != crate::domain::scheduler_protocol::ScenarioMode::Authoritative
        }) {
            return Ok(0);
        }
        let agent_id = self.inner.agent.lock().await.state.id.clone();
        let Some(snapshot) = self
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized(&agent_id)?
        else {
            return Ok(0);
        };
        let claimed = self
            .inner
            .runtime_db
            .queue_entries()
            .recent(Some(&agent_id), usize::MAX)?
            .into_iter()
            .filter(|entry| entry.status == QueueEntryStatus::Dequeued)
            .collect::<Vec<_>>();
        let turns = self
            .inner
            .runtime_db
            .turn_records()
            .recent_for_agent(&agent_id, usize::MAX)?;
        let mut recovered = 0;

        for mut entry in claimed {
            let expected_entry = entry.clone();
            let activation_id = scheduler_executor::canonical_activation_id(&entry.message_id);
            let Some(activation) = snapshot.activations.get(&activation_id) else {
                continue;
            };
            let Some(message) = self.inner.storage.read_message_by_id(&entry.message_id)? else {
                continue;
            };
            if !matches!(
                (&message.kind, &message.origin),
                (MessageKind::SystemTick, MessageOrigin::System { subsystem })
                    if subsystem == "work_queue"
            ) || message.work_item_id.as_deref() != Some(activation.work_item_id.as_str())
            {
                continue;
            }

            let terminal_turn = turns.iter().find(|turn| {
                turn.terminal.is_some()
                    && turn
                        .trigger
                        .as_ref()
                        .and_then(|trigger| trigger.message_id.as_deref())
                        == Some(entry.message_id.as_str())
                    && message.turn_id.as_deref() == Some(turn.turn_id.as_str())
                    && turn.current_work_item_id.as_deref()
                        == Some(activation.work_item_id.as_str())
            });
            let terminal_is_completed = terminal_turn.is_some_and(|turn| {
                turn.terminal.as_ref().is_some_and(|terminal| {
                    terminal.kind == crate::types::TurnTerminalKind::Completed
                })
            });
            // Canonical terminal states need no new protocol command; these
            // branches only reconcile the legacy queue claim that remains.
            if activation.state == crate::domain::scheduler_protocol::ActivationState::Settled {
                entry.status = if terminal_is_completed {
                    QueueEntryStatus::Processed
                } else {
                    QueueEntryStatus::Aborted
                };
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let queue_status = entry.status.clone();
                let commit = self.inner.runtime_db.transitions().commit_queue(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Settle,
                        mutation:
                            crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                expected: expected_entry,
                                record: entry,
                            },
                        scheduler_claim_work_item: None,
                        scheduler_protocol_bootstrap: None,
                        scheduler_protocol_commands: Vec::new(),
                        scheduler_authority_scenarios: Vec::new(),
                        scheduler_rollout_expectations: Vec::new(),
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: terminal_turn.cloned(),
                        audit_events: vec![AuditEvent::legacy(
                            "scheduler_bootstrap_claim_recovered",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "message_id": message_id,
                                "activation_id": activation_id,
                                "work_item_id": activation.work_item_id,
                                "queue_status": queue_status,
                                "recovery_outcome": "legacy_queue_reconciled_from_canonical_settlement",
                                "terminal_turn_id": terminal_turn.map(|turn| turn.turn_id.clone()),
                                "provenance": "bootstrap_reconciliation",
                            }),
                        )],
                        scheduler_shadow_comparison: None,
                        scheduler_delivery_shadow_comparison: None,
                        scheduler_semantic_shadow: None,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                )?;
                if commit.applied {
                    recovered += 1;
                }
                self.apply_transition_commit(commit).await;
                continue;
            }
            if activation.state
                == crate::domain::scheduler_protocol::ActivationState::SettlementMissing
            {
                entry.status = QueueEntryStatus::Aborted;
                entry.updated_at = self.now();
                let message_id = entry.message_id.clone();
                let commit = self.inner.runtime_db.transitions().commit_queue(
                    &crate::runtime_db::transitions::QueueTransitionCommand {
                        agent_id: agent_id.clone(),
                        operation: crate::runtime_db::transitions::QueueOperation::Settle,
                        mutation:
                            crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                                expected: expected_entry,
                                record: entry,
                            },
                        scheduler_claim_work_item: None,
                        scheduler_protocol_bootstrap: None,
                        scheduler_protocol_commands: Vec::new(),
                        scheduler_authority_scenarios: Vec::new(),
                        scheduler_rollout_expectations: Vec::new(),
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: terminal_turn.cloned(),
                        audit_events: vec![AuditEvent::legacy(
                            "scheduler_bootstrap_claim_recovered",
                            serde_json::json!({
                                "agent_id": agent_id,
                                "message_id": message_id,
                                "activation_id": activation_id,
                                "work_item_id": activation.work_item_id,
                                "queue_status": QueueEntryStatus::Aborted,
                                "recovery_outcome": "legacy_queue_reconciled_from_canonical_missing_settlement",
                                "terminal_turn_id": terminal_turn.map(|turn| turn.turn_id.clone()),
                                "provenance": "bootstrap_reconciliation",
                            }),
                        )],
                        scheduler_shadow_comparison: None,
                        scheduler_delivery_shadow_comparison: None,
                        scheduler_semantic_shadow: None,
                        notify_scheduler: true,
                        fault: self.take_transition_fault(),
                        brief_evidence: Vec::new(),
                    },
                )?;
                if commit.applied {
                    recovered += 1;
                }
                self.apply_transition_commit(commit).await;
                continue;
            }
            entry.status = if terminal_is_completed {
                QueueEntryStatus::Processed
            } else {
                QueueEntryStatus::Aborted
            };
            entry.updated_at = self.now();
            let mut scheduler_protocol_commands = if terminal_is_completed {
                self.canonical_queue_settlement_commands(&entry).await?
            } else {
                vec![
                    crate::domain::scheduler_protocol::ProtocolCommand::RecordMissingSettlement(
                        crate::domain::scheduler_protocol::MissingSettlementRecord {
                            id: canonical_missing_settlement_id(&entry.message_id),
                            activation_id: activation_id.clone(),
                            created_at: entry.updated_at.to_rfc3339(),
                        },
                    ),
                ]
            };
            let settles_from_terminal = matches!(
                scheduler_protocol_commands.as_slice(),
                [crate::domain::scheduler_protocol::ProtocolCommand::SettleActivation(_)]
            );
            if !settles_from_terminal {
                entry.status = QueueEntryStatus::Aborted;
                if terminal_is_completed {
                    scheduler_protocol_commands = canonical_queue_settlement_commands_from_facts(
                        &self.inner.storage,
                        &self.inner.runtime_db,
                        &entry,
                    )?;
                }
            }
            let rejected = scheduler_protocol_commands.iter().find_map(|command| {
                let outcome = crate::domain::scheduler_protocol::reduce_command(&snapshot, command);
                (outcome.outcome.decision == crate::domain::scheduler_protocol::Decision::Rejected)
                    .then(|| outcome.outcome.diagnostics)
            });
            if let Some(diagnostics) = rejected {
                self.inner.storage.append_event(
                    &scheduler::scheduler_invariant_diagnostic_event(
                        &agent_id,
                        "bootstrap_recovery_command_rejected",
                        Some(activation.work_item_id.clone()),
                        Some(entry.message_id.clone()),
                        diagnostics,
                    )?,
                )?;
                continue;
            }
            let message_id = entry.message_id.clone();
            let work_item_id = activation.work_item_id.clone();
            let queue_status = entry.status.clone();
            let terminal_turn_id = terminal_turn.map(|turn| turn.turn_id.clone());
            let recovery_outcome = if settles_from_terminal {
                "settled_from_terminal_turn"
            } else {
                "settlement_missing"
            };
            let commit = self.inner.runtime_db.transitions().commit_queue(
                &crate::runtime_db::transitions::QueueTransitionCommand {
                    agent_id: agent_id.clone(),
                    operation: crate::runtime_db::transitions::QueueOperation::Settle,
                    mutation: crate::runtime_db::transitions::QueueMutation::CompareAndSet {
                        expected: expected_entry,
                        record: entry,
                    },
                    scheduler_claim_work_item: None,
                    scheduler_protocol_bootstrap: None,
                    scheduler_protocol_commands,
                    scheduler_authority_scenarios: Vec::new(),
                    scheduler_rollout_expectations: scheduler_rollout_expectations.clone(),
                    agent_state: None,
                    message_evidence: Vec::new(),
                    transcript_entries: Vec::new(),
                    turn_record: terminal_turn.cloned(),
                    audit_events: vec![AuditEvent::legacy(
                        "scheduler_bootstrap_claim_recovered",
                        serde_json::json!({
                            "agent_id": agent_id,
                            "message_id": message_id,
                            "activation_id": activation_id,
                            "work_item_id": work_item_id,
                            "queue_status": queue_status,
                            "recovery_outcome": recovery_outcome,
                            "terminal_turn_id": terminal_turn_id,
                            "provenance": "bootstrap_reconciliation",
                        }),
                    )],
                    scheduler_shadow_comparison: None,
                    scheduler_delivery_shadow_comparison: None,
                    scheduler_semantic_shadow: None,
                    notify_scheduler: true,
                    fault: self.take_transition_fault(),
                    brief_evidence: Vec::new(),
                },
            )?;
            if commit.applied {
                recovered += 1;
            }
            self.apply_transition_commit(commit).await;
        }
        Ok(recovered)
    }

    async fn record_scheduler_bootstrap_diagnostics(&self) -> Result<()> {
        const STUCK_ACTIVATION_AGE: chrono::Duration = chrono::Duration::minutes(5);

        let (agent_id, active_run_id) = {
            let guard = self.inner.agent.lock().await;
            (guard.state.id.clone(), guard.state.current_run_id.clone())
        };
        let Some(snapshot) = self
            .inner
            .runtime_db
            .transitions()
            .load_scheduler_protocol_snapshot_if_initialized(&agent_id)?
        else {
            return Ok(());
        };
        let queue_entries = self
            .inner
            .runtime_db
            .queue_entries()
            .recent(Some(&agent_id), usize::MAX)?;
        let turns = self
            .inner
            .runtime_db
            .turn_records()
            .recent_for_agent(&agent_id, usize::MAX)?;
        let work_items = self
            .inner
            .runtime_db
            .work_items()
            .latest_for_agent(&agent_id, usize::MAX)?;
        let mut events = Vec::new();

        for (activation_id, activation) in snapshot.activations.iter().filter(|(_, activation)| {
            activation.state == crate::domain::scheduler_protocol::ActivationState::Running
        }) {
            let message_id = activation_id
                .strip_prefix("activation:message:")
                .map(ToString::to_string);
            let queue_entry = message_id.as_deref().and_then(|message_id| {
                queue_entries
                    .iter()
                    .find(|entry| entry.message_id == message_id)
            });
            let terminal_turn = message_id.as_deref().and_then(|message_id| {
                turns.iter().find(|turn| {
                    turn.terminal.is_some()
                        && turn
                            .trigger
                            .as_ref()
                            .and_then(|trigger| trigger.message_id.as_deref())
                            == Some(message_id)
                })
            });
            let mut base_evidence = vec![
                format!("activation_id={activation_id}"),
                format!("work_item_id={}", activation.work_item_id),
                format!("admitted_generation={}", activation.admitted_generation),
            ];
            if active_run_id.is_none() {
                events.push(scheduler::scheduler_invariant_diagnostic_event(
                    &agent_id,
                    "running_activation_without_active_run",
                    Some(activation.work_item_id.clone()),
                    message_id.clone(),
                    base_evidence.clone(),
                )?);
            }
            if work_items.iter().any(|work_item| {
                work_item.id == activation.work_item_id
                    && work_item.state == crate::types::WorkItemState::Completed
            }) {
                events.push(scheduler::scheduler_invariant_diagnostic_event(
                    &agent_id,
                    "completed_work_item_has_running_activation",
                    Some(activation.work_item_id.clone()),
                    message_id.clone(),
                    base_evidence.clone(),
                )?);
            }
            if let (Some(queue_entry), Some(turn)) = (queue_entry, terminal_turn) {
                if queue_entry.status == QueueEntryStatus::Dequeued {
                    let mut evidence = base_evidence.clone();
                    evidence.push(format!("turn_id={}", turn.turn_id));
                    evidence.push("queue_status=dequeued".into());
                    events.push(scheduler::scheduler_invariant_diagnostic_event(
                        &agent_id,
                        "terminal_turn_has_dequeued_queue",
                        Some(activation.work_item_id.clone()),
                        message_id.clone(),
                        evidence,
                    )?);
                }
            }
            if let Some(queue_entry) = queue_entry {
                let age = self.now().signed_duration_since(queue_entry.updated_at);
                if age >= STUCK_ACTIVATION_AGE {
                    base_evidence.push(format!("age_seconds={}", age.num_seconds()));
                    events.push(scheduler::scheduler_invariant_diagnostic_event(
                        &agent_id,
                        "running_activation_age_exceeded",
                        Some(activation.work_item_id.clone()),
                        message_id,
                        base_evidence,
                    )?);
                }
            }
        }

        if !events.is_empty() {
            tracing::warn!(
                agent_id = %agent_id,
                diagnostic_count = events.len(),
                "scheduler bootstrap invariants require attention"
            );
            let recent_events = self.inner.storage.read_recent_events(128)?;
            events.retain(|event| {
                !recent_events
                    .iter()
                    .any(|recent| recent.kind == event.kind && recent.data == event.data)
            });
        }
        if !events.is_empty() {
            self.inner.storage.append_events(&events)?;
        }
        Ok(())
    }

    async fn bootstrap_recovery(&self) -> Result<()> {
        if let Some(tasks) = self.inner.recovered_tasks.lock().await.take() {
            let (reattached, interrupted_tasks) =
                self.recover_supervised_child_tasks(tasks).await?;
            let interrupted = self.interrupt_active_tasks(interrupted_tasks).await?;
            if !reattached.is_empty() {
                self.inner.storage.append_event(&AuditEvent::legacy(
                    "supervised_child_task_monitor_reattached",
                    serde_json::json!({
                        "agent_id": self.agent_id().await?,
                        "task_ids": reattached.iter().map(|task| task.id.clone()).collect::<Vec<_>>(),
                    }),
                ))?;
            }
            if !interrupted.is_empty() {
                self.emit_system_tick_from_interrupted_tasks(&interrupted)
                    .await?;
            }
        }
        if let Some(timers) = self.inner.recovered_timers.lock().await.take() {
            self.recover_active_timers(timers).await?;
        }
        self.emit_recovered_pending_wake_hint().await?;
        Ok(())
    }
}

fn decode_exec_command_batch_result(value: &serde_json::Value) -> Option<ExecCommandBatchResult> {
    let mut value = value.clone();
    if let serde_json::Value::Object(map) = &mut value {
        map.entry("summary_text").or_insert(serde_json::Value::Null);
        if let Some(serde_json::Value::Array(items)) = map.get_mut("items") {
            for item in items {
                if let serde_json::Value::Object(item) = item {
                    if let Some(serde_json::Value::Object(result)) = item.get_mut("result") {
                        result
                            .entry("summary_text")
                            .or_insert(serde_json::Value::Null);
                    }
                }
            }
        }
    }
    serde_json::from_value(value).ok()
}

fn command_mentions_path(command: &str, path: &Path) -> bool {
    let display = path.to_string_lossy();
    command.contains(display.as_ref())
}

fn command_skill_activation(
    command: &str,
    skill: &SkillCatalogEntry,
    workspace_anchor: &Path,
) -> Option<(PathBuf, SkillLoadReason)> {
    if command_mentions_path(command, &skill.path)
        || skill
            .path
            .strip_prefix(workspace_anchor)
            .map(|relative| command_mentions_path(command, relative))
            .unwrap_or(false)
    {
        return Some((skill.path.clone(), SkillLoadReason::ReadSkillMd));
    }

    let skill_root = skill.path.parent()?;
    let scripts_root = skill_root.join("scripts");
    for script_path in script_paths_under(&scripts_root) {
        if command_mentions_path(command, &script_path)
            || script_path
                .strip_prefix(workspace_anchor)
                .map(|relative| command_mentions_path(command, relative))
                .unwrap_or(false)
        {
            return Some((script_path, SkillLoadReason::RunSkillScript));
        }
    }

    if command_mentions_path(command, &scripts_root)
        || scripts_root
            .strip_prefix(workspace_anchor)
            .map(|relative| command_mentions_path(command, relative))
            .unwrap_or(false)
    {
        return Some((scripts_root, SkillLoadReason::RunSkillScript));
    }

    None
}

fn script_paths_under(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    collect_script_paths(root, &mut paths);
    paths
}

fn collect_script_paths(path: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.is_file() {
        paths.push(path.to_path_buf());
        return;
    }
    if !metadata.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten() {
        collect_script_paths(&entry.path(), paths);
    }
}

fn skill_for_activation_path<'a>(
    skills: &'a [SkillCatalogEntry],
    path: &Path,
) -> Option<&'a SkillCatalogEntry> {
    find_skill_by_entrypoint(skills, path).or_else(|| find_skill_by_script_path(skills, path))
}

#[cfg(test)]
fn current_input_summary(effective_prompt: &EffectivePrompt) -> String {
    let current_input = effective_prompt
        .context_sections
        .iter()
        .find(|section| section.name == "current_input")
        .map(|section| section.content.as_str())
        .unwrap_or_default();

    current_input
        .lines()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .trim_start_matches("- ")
        .rsplit_once("] ")
        .map(|(_, body)| body.to_string())
        .unwrap_or_else(|| current_input.to_string())
}

fn combine_text_history(history: &[String], text_blocks: &[String]) -> Vec<String> {
    history
        .iter()
        .cloned()
        .chain(text_blocks.iter().cloned())
        .collect()
}

fn is_max_output_stop_reason(stop_reason: Option<&str>) -> bool {
    matches!(
        stop_reason,
        Some("max_tokens") | Some("max_output_tokens") | Some("model_context_window_exceeded")
    )
}

fn normalized_turn_id(turn_id: Option<&str>) -> Option<String> {
    turn_id
        .map(str::trim)
        .filter(|turn_id| !turn_id.is_empty())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests;
