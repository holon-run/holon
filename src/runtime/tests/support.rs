#![allow(dead_code, unused_imports)]

pub(crate) use super::runtime_state::recover_quiescent_startup_fixture;
pub(crate) use async_trait::async_trait;
pub(crate) use chrono::Utc;
pub(crate) use std::path::PathBuf;
pub(crate) use std::sync::Arc;
pub(crate) use tempfile::{tempdir, TempDir};
pub(crate) use tokio::runtime::Runtime;
pub(crate) use tokio::sync::Mutex;

pub(crate) use crate::{
    config::AppConfig,
    context::ContextConfig,
    host::RuntimeHost,
    prompt::{render_section, PromptSection, PromptStability},
    provider::{
        provider_transport_error_with_code, provider_turn_error, AgentProvider,
        ConversationMessage, ModelBlock, ProviderAttemptOutcome, ProviderAttemptRecord,
        ProviderAttemptTimeline, ProviderFailureClassification, ProviderFailureKind,
        ProviderHttpTraceDiagnostics, ProviderTransportDiagnostics, ProviderTransportTimeline,
        ProviderTurnRequest, ProviderTurnResponse, ReqwestTransportDiagnostics, RetryDisposition,
        StubProvider,
    },
    storage::AppStorage,
    system::{ExecutionProfile, ExecutionSnapshot, WorkspaceAccessMode, WorkspaceProjectionKind},
    types::{
        ActiveWorkspaceEntry, AgentIdentityView, AgentKind, AgentOwnership, AgentProfilePreset,
        AgentRegistryStatus, AgentState, AgentStatus, AgentVisibility, AuditEvent, AuthorityClass,
        BriefKind, BriefRecord, CallbackDeliveryMode, ClosureDecision, ClosureOutcome,
        ContinuationClass, ContinuationTriggerKind, DeliverySummaryRecord, LoadedAgentMemory,
        LoadedAgentsMd, MessageBody, MessageDeliverySurface, MessageKind, MessageOrigin,
        PendingWakeHint, Priority, QueueEntryStatus, TaskKind, TaskOutputRetrievalStatus,
        TaskRecord, TaskRecoverySpec, TaskStatus, TimerRecord, TimerStatus, TodoItem,
        TodoItemState, TokenUsage, TurnRecord, TurnTerminalKind, TurnTerminalRecord, WaitingReason,
        WorkItemRecord, WorkItemState, WorkReactivationMode, WorkspaceEntry,
    },
};

use super::super::*;

mod lifecycle;
pub(crate) use lifecycle::{
    advance_lifecycle_time, assert_injected_transition_fault, controlled_clock,
    DurableLifecycleSnapshot, LifecycleHarness, POST_COMMIT_FAULTS, PRE_COMMIT_FAULTS,
    TERMINAL_PRE_COMMIT_FAULTS,
};

/// Seed Queue progress through the original writer, including its Current ACK.
pub(crate) async fn persist_native_queue_fixture(
    runtime: &RuntimeHandle,
    record: crate::types::QueueEntryRecord,
) {
    use crate::runtime_db::transitions::{
        PreparedQueueTransition, QueueMutation, QueueOperation, QueueTransitionCommand,
    };
    let statuses = match record.status {
        QueueEntryStatus::Dequeued | QueueEntryStatus::Interjected => {
            vec![QueueEntryStatus::Queued, record.status.clone()]
        }
        _ => vec![record.status.clone()],
    };
    for status in statuses {
        let mut entry = record.clone();
        entry.status = status;
        let (operation, mutation) = match entry.status {
            QueueEntryStatus::Queued => (QueueOperation::Admit, QueueMutation::Upsert(entry)),
            QueueEntryStatus::Dequeued => (QueueOperation::Claim, QueueMutation::Consume(entry)),
            QueueEntryStatus::Interjected => {
                (QueueOperation::Interject, QueueMutation::Consume(entry))
            }
            QueueEntryStatus::Processed => (QueueOperation::Settle, QueueMutation::Upsert(entry)),
            _ => panic!("unsupported native Queue fixture status"),
        };
        let commit = {
            let mut mutation_guard = runtime.begin_mutation().await.unwrap();
            if matches!(operation, QueueOperation::Admit) {
                let (_, record_limit, byte_limit) =
                    mutation_guard.context_input_evidence_policy().unwrap();
                let request =
                    crate::runtime_db::input_evidence_snapshot::InputEvidenceLoadRequest {
                        query: None,
                        windows: Vec::new(),
                        mandatory_roots: std::collections::BTreeSet::from([
                            crate::runtime::resident_input_evidence::InputEvidenceKey::new(
                                crate::runtime::resident_input_evidence::InputEvidenceFamily::Message,
                                &record.message_id,
                            ),
                        ]),
                        interrupted_sources: Default::default(),
                        selected_completion_brief_ids: Default::default(),
                        record_limit,
                        byte_limit,
                    };
                mutation_guard
                    .prepare_owner_input_request(&request, 0)
                    .await
                    .unwrap();
            }
            mutation_guard
                .commit_prepared_queue(
                    crate::ids::audit_event_id(),
                    PreparedQueueTransition::new(QueueTransitionCommand {
                        agent_id: record.agent_id.clone(),
                        operation,
                        mutation,
                        scheduler_claim_work_item: None,
                        agent_state: None,
                        message_evidence: Vec::new(),
                        transcript_entries: Vec::new(),
                        turn_record: None,
                        audit_events: Vec::new(),
                        notify_scheduler: false,
                        fault: None,
                        brief_evidence: Vec::new(),
                    }),
                )
                .await
                .unwrap()
        };
        assert!(commit.applied);
        runtime.apply_transition_commit(commit).await;
    }
}

/// Prompt budget estimated from the live PublicNamed tool registry, matching
/// `context_config()`. Tool descriptions drift over time and shift this
/// budget, so tests with size-sensitive fixtures should scale their fixtures
/// from this value instead of pinning absolute sizes.
pub(crate) fn public_tools_prompt_budget_estimated_tokens() -> usize {
    let available_tools =
        crate::tool::ToolRegistry::new(PathBuf::from("/tmp/holon-test-workspace"))
            .tool_specs_with_families()
            .unwrap()
            .into_iter()
            .filter(|(family, _)| {
                AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
            })
            .filter(|(_, tool)| tool.name != crate::tool::names::X_SEARCH)
            .map(|(_, tool)| tool)
            .collect::<Vec<_>>();
    super::super::turn::estimate_tool_specs_tokens(&available_tools) + 4096
}

pub(crate) fn context_config() -> ContextConfig {
    let prompt_budget_estimated_tokens = public_tools_prompt_budget_estimated_tokens();
    ContextConfig {
        recent_messages: 8,
        recent_briefs: 8,
        compaction_trigger_messages: 10,
        compaction_keep_recent_messages: 4,
        prompt_budget_estimated_tokens,
        turn_projection_min_budget: prompt_budget_estimated_tokens,
        ..ContextConfig::default()
    }
}

pub(crate) fn isolate_test_user_home(runtime: &RuntimeHandle) {
    let mut config = (*runtime.inner.config_snapshot.load_full()).clone();
    config.user_home_dir = None;
    runtime.inner.config_snapshot.store(Arc::new(config));
}

pub(crate) fn continuation_ready_context_config(
    workspace: &TempDir,
    continuation_effective_budget: usize,
) -> ContextConfig {
    let available_tools = crate::tool::ToolRegistry::new(workspace.path().to_path_buf())
        .tool_specs_with_families()
        .unwrap()
        .into_iter()
        .filter(|(family, _)| {
            AgentProfilePreset::PublicNamed.allows_tool_capability_family(*family)
        })
        .filter(|(_, tool)| tool.name != crate::tool::names::X_SEARCH)
        .map(|(_, tool)| tool)
        .collect::<Vec<_>>();
    let prompt_budget_estimated_tokens =
        super::super::turn::estimate_tool_specs_tokens(&available_tools)
            + super::super::turn::CONTINUATION_BUDGET_SAFETY_MARGIN_TOKENS
            + continuation_effective_budget;
    ContextConfig {
        prompt_budget_estimated_tokens,
        turn_projection_budget_ratio: 1.0,
        turn_projection_min_budget: 0,
        turn_projection_max_budget: prompt_budget_estimated_tokens,
        callback_base_url: String::new(),
        ..context_config()
    }
}

/// Crate-level fixtures use the same qualified Task writer as runtime tests.
pub(crate) async fn persist_task_transition(
    runtime: &RuntimeHandle,
    record: &TaskRecord,
    event_type: &'static str,
) -> anyhow::Result<()> {
    runtime.persist_task_transition(record, event_type).await
}

/// Persist the original Timer creation ACK, leaving its loop unarmed so restart
/// is the first producer. This models a crash after durable creation.
pub(crate) async fn persist_timer_creation_for_restart(
    runtime: &RuntimeHandle,
    record: &TimerRecord,
) -> anyhow::Result<()> {
    let mut mutation = runtime.begin_mutation().await?;
    let prepared = crate::runtime_db::transitions::PreparedInputTransition::timer(
        mutation.input_evidence_scope(),
        crate::ids::audit_event_id(),
        runtime.now(),
        crate::runtime_db::transitions::PreparedTimerTransition::create(record.clone(), None),
    )?;
    let (commit, outcome) = mutation.commit_prepared_timer_transition(prepared).await?;
    assert!(matches!(outcome,
        crate::runtime_db::transitions::TimerMutationOutcome::Created { record: actual }
        if actual == *record));
    drop(mutation);
    runtime.apply_transition_commit(commit).await;
    Ok(())
}

/// Historical parent evidence is committed through the original evidence writer;
/// it does not stand in for an execution attempt or an active parent dispatch.
pub(crate) async fn persist_completed_task_parent_fixture(
    runtime: &RuntimeHandle,
    turn_id: &str,
    work_item_id: Option<&str>,
) -> anyhow::Result<()> {
    let mut mutation = runtime.begin_mutation().await?;
    let at = runtime.now();
    let mut record = TurnRecord::new(
        &mutation.state.id,
        turn_id,
        mutation.state.turn_index.max(1),
    );
    record.created_at = at;
    record.current_work_item_id = work_item_id.map(str::to_owned);
    record.terminal = Some(crate::types::TurnTerminalSummary {
        kind: TurnTerminalKind::Completed,
        reason: Some("historical_task_parent_fixture".into()),
        no_brief_reason: None,
        completed_at: at,
        duration_ms: 0,
    });
    let prepared = crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::turn(
        mutation.input_evidence_scope(),
        crate::ids::audit_event_id(),
        at,
        record,
    )?;
    mutation.commit_prepared_evidence(prepared, None).await?;
    Ok(())
}

pub(crate) async fn finish_claimed_test_run(runtime: &RuntimeHandle) {
    let mut guard = runtime.inner.agent.lock().await;
    // Provider polling has ended; the original queue terminal transaction owns
    // durable Run/State closure and its dispatch Memory settlement.
    guard.current_run_abort = None;
}

pub(crate) fn terminal_transition(
    message: &MessageEnvelope,
    work_item_id: Option<&str>,
) -> super::super::turn::TurnTerminalTransition {
    let turn_id = message.turn_id.clone().expect("test message turn id");
    let terminal = TurnTerminalRecord {
        turn_id: turn_id.clone(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("terminal transition committed".into()),
        no_brief_reason: Some(crate::types::TurnNoBriefReason::ReducerOnly {
            reason: "test_terminal_transition".into(),
        }),
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 1,
    };
    let mut turn_record = TurnRecord::new(&message.agent_id, turn_id, 1);
    turn_record.current_work_item_id = work_item_id.map(ToString::to_string);
    turn_record.trigger = Some(crate::types::TurnTriggerSummary::from_message(message));
    turn_record.input_message_ids = vec![message.id.clone()];
    turn_record.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(&terminal));
    super::super::turn::TurnTerminalTransition {
        terminal,
        turn_record: super::super::turn::inputs::BuiltTurnRecord::fixture(turn_record),
        prepared_work_item_completion: None,
        prepared_wait_for: None,
        provider_recovery_message: None,
        terminal_tool_executions: Vec::new(),
    }
}

pub(crate) fn terminal_transition_from_started_turn(
    runtime: &RuntimeHandle,
    message: &MessageEnvelope,
) -> super::super::turn::TurnTerminalTransition {
    let mut record = runtime
        .storage()
        .read_turn_by_id(message.turn_id.as_deref().unwrap())
        .unwrap()
        .expect("turn identity was persisted at admission");
    let mut transition = terminal_transition(message, record.current_work_item_id.as_deref());
    transition.terminal.turn_index = record.turn_index;
    record.terminal = Some(crate::types::TurnTerminalSummary::from_terminal(
        &transition.terminal,
    ));
    transition.turn_record = super::super::turn::inputs::BuiltTurnRecord::fixture(record);
    transition
}

pub(crate) async fn wait_for_audit_events(
    runtime: &RuntimeHandle,
    limit: usize,
    mut predicate: impl FnMut(&[AuditEvent]) -> bool,
    label: &str,
) -> Vec<AuditEvent> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let events = runtime.storage().read_recent_events(limit).unwrap();
            if predicate(&events) {
                return events;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"))
}

pub(crate) async fn host_backed_test_runtime() -> (TempDir, RuntimeHost, RuntimeHandle) {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let mut config =
        crate::config::AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    config.user_home_dir = None;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("done"))).unwrap();
    let runtime = host.default_runtime().await.unwrap();
    (home, host, runtime)
}

pub(crate) fn private_child_identity(agent_id: &str) -> AgentIdentityView {
    AgentIdentityView {
        agent_id: agent_id.into(),
        name: None,
        kind: AgentKind::Child,
        visibility: AgentVisibility::Private,
        ownership: AgentOwnership::ParentSupervised,
        profile_preset: AgentProfilePreset::PrivateChild,
        can_rename: false,
        status: AgentRegistryStatus::Active,
        is_default_agent: false,
        incarnation: 1,
        parent_agent_id: Some("default".into()),
        lineage_parent_agent_id: Some("default".into()),
        delegated_from_task_id: Some("task-1".into()),
    }
}

pub(crate) fn test_effective_prompt() -> EffectivePrompt {
    EffectivePrompt {
        identity: AgentIdentityView {
            agent_id: "default".into(),
            name: None,
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            can_rename: false,
            status: AgentRegistryStatus::Active,
            is_default_agent: true,
            incarnation: 1,
            parent_agent_id: None,
            lineage_parent_agent_id: None,
            delegated_from_task_id: None,
        },
        agent_home: PathBuf::from("/tmp/agent-home"),
        execution: ExecutionSnapshot {
            profile: ExecutionProfile::default(),
            policy: ExecutionProfile::default().policy_snapshot(),
            attached_workspaces: vec![],
            workspace_id: None,
            workspace_anchor: PathBuf::from("/tmp/agent-home"),
            execution_root: PathBuf::from("/tmp/agent-home"),
            cwd: PathBuf::from("/tmp/agent-home"),
            execution_root_id: None,
            projection_kind: None,
            access_mode: None,
            worktree_root: None,
            execution_roots: Vec::new(),
        },
        loaded_agents_md: LoadedAgentsMd::default().into(),
        loaded_agent_memory: LoadedAgentMemory::default().into(),
        cache_identity: crate::prompt::PromptCacheIdentity {
            agent_id: "default".into(),
            prompt_cache_key: "default".into(),
            context_fingerprint: "fingerprint-default".into(),
            compression_epoch: 0,
        },
        system_sections: vec![],
        context_sections: vec![],
        rendered_system_prompt: "system".into(),
        rendered_context_attachment: "context".into(),
        projection_owner: crate::projection_eval::ProjectionOwner::AgentLifecycle {
            agent_id: "default".into(),
        },
        projection_binding: None,
        projection_turn_id: None,
        projection_evidence: Default::default(),
        context_plan_evidence: Default::default(),
        recent_turns_reprojection: None,
        frozen_history_policy: crate::context::ContextHistoryPreparation::default(),
        frozen_projection_budget: None,
        frozen_recent_message_limit: None,
        _resource_read_material: None,
    }
}

pub(crate) fn closure_decision(
    outcome: ClosureOutcome,
    waiting_reason: Option<WaitingReason>,
) -> ClosureDecision {
    ClosureDecision {
        outcome,
        waiting_reason,
        work_signal: None,
        runtime_posture: RuntimePosture::Awake,
        evidence: Vec::new(),
    }
}

pub(crate) async fn bind_turn_to_work_item(runtime: &RuntimeHandle, work_item_id: &str) {
    let mut guard = runtime.begin_mutation().await.unwrap();
    guard.state.turn_index = 1;
    guard.state.current_turn_work_item_id = Some(work_item_id.to_string());
    guard.state.last_turn_terminal = Some(TurnTerminalRecord {
        turn_id: "test-turn".into(),
        turn_index: 1,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("done".into()),
        no_brief_reason: None,
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 10,
    });
    guard.persist_state(&runtime.inner.storage).await.unwrap();
}

pub(crate) fn bind_autonomous_work_queue_tick(
    message: &mut MessageEnvelope,
    work_item: &WorkItemRecord,
    reason: &str,
) {
    message.work_item_id = Some(work_item.id.clone());
    message.metadata = Some(serde_json::json!({
        "work_queue": {
            "reason": reason,
            "work_item_id": work_item.id,
            "work_item_revision": work_item.revision
        }
    }));
}

pub(crate) fn seed_runnable_work_execution(storage: &AppStorage, work_item: &WorkItemRecord) {
    seed_work_execution(
        storage,
        work_item,
        crate::domain::execution_protocol::WorkItemExecutionState::Runnable {
            generation: work_item.revision.max(1),
            recovery_ref: None,
        },
    );
}

/// Seed a temporarily non-runnable lane whose work item still projects as
/// runnable (paused execution, no reciprocal attempt requirements).
pub(crate) fn seed_paused_work_execution(
    storage: &AppStorage,
    work_item: &WorkItemRecord,
    reason: &str,
) {
    seed_work_execution(
        storage,
        work_item,
        crate::domain::execution_protocol::WorkItemExecutionState::Paused {
            generation: work_item.revision.max(1),
            reason: reason.into(),
        },
    );
}

pub(crate) fn seed_waiting_work_execution(
    storage: &AppStorage,
    work_item: &WorkItemRecord,
    wait_id: &str,
) {
    seed_work_execution(
        storage,
        work_item,
        crate::domain::execution_protocol::WorkItemExecutionState::Waiting {
            generation: work_item.revision.max(1),
            wait: crate::domain::execution_protocol::WaitReference {
                wait_id: wait_id.into(),
            },
        },
    );
}

/// The caller has unloaded the Host runtime before this exact fixture write.
/// Plain pre-bootstrap fixtures above intentionally keep their legacy path.
pub(crate) fn seed_waiting_work_execution_for_unloaded_host(
    storage: &AppStorage,
    work_item: &WorkItemRecord,
    wait_id: &str,
) {
    use crate::domain::execution_protocol::{WorkItemExecutionRecord, WorkItemExecutionState};
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    use crate::runtime_db::input_evidence_snapshot::{
        register_committed_input_evidence_tx, CanonicalInputEvidence,
    };
    use crate::runtime_db::transitions::execution_protocol_repository::{
        load_state_unchecked_tx, persist_repaired_state_tx,
    };

    let runtime_db = storage
        .runtime_db()
        .unwrap()
        .expect("test storage should expose RuntimeDb");
    runtime_db
        .transaction(|tx| {
            let identity: crate::types::AgentIdentityRecord =
                serde_json::from_str(&tx.query_row(
                    "SELECT payload_json FROM agent_identities WHERE agent_id=?1",
                    [&work_item.agent_id],
                    |row| row.get::<_, String>(0),
                )?)?;
            let scope = InputEvidenceScope {
                agent_id: work_item.agent_id.clone(),
                identity_incarnation: identity.incarnation,
            };
            let previous = load_state_unchecked_tx(tx, &work_item.agent_id)?;
            let mut execution = previous.clone();
            execution.work_items.insert(
                work_item.id.clone(),
                WorkItemExecutionRecord {
                    source_revision: work_item.revision,
                    state: WorkItemExecutionState::Waiting {
                        generation: work_item.revision.max(1),
                        wait: crate::domain::execution_protocol::WaitReference {
                            wait_id: wait_id.into(),
                        },
                    },
                },
            );
            persist_repaired_state_tx(tx, &previous, &execution)?;
            let actual: WorkItemExecutionRecord = serde_json::from_str(&tx.query_row(
                "SELECT payload_json FROM execution_protocol_work_items
                 WHERE agent_id=?1 AND work_item_id=?2",
                rusqlite::params![work_item.agent_id, work_item.id],
                |row| row.get::<_, String>(0),
            )?)?;
            register_committed_input_evidence_tx(
                tx,
                &scope,
                &[CanonicalInputEvidence::WorkExecution {
                    work_item_id: work_item.id.clone(),
                    record: actual,
                }],
            )?;
            Ok(())
        })
        .unwrap();
}

/// Live fixtures must ACK the touched lane in its original write transaction.
pub(crate) async fn seed_live_runnable_work_execution(
    runtime: &RuntimeHandle,
    work_item: &WorkItemRecord,
) {
    seed_live_work_execution(
        runtime,
        work_item,
        crate::domain::execution_protocol::WorkItemExecutionState::Runnable {
            generation: work_item.revision.max(1),
            recovery_ref: None,
        },
    )
    .await;
}

pub(crate) async fn seed_live_paused_work_execution(
    runtime: &RuntimeHandle,
    work_item: &WorkItemRecord,
    reason: &str,
) {
    seed_live_work_execution(
        runtime,
        work_item,
        crate::domain::execution_protocol::WorkItemExecutionState::Paused {
            generation: work_item.revision.max(1),
            reason: reason.to_owned(),
        },
    )
    .await;
}

pub(crate) async fn seed_live_waiting_work_execution(
    runtime: &RuntimeHandle,
    work_item: &WorkItemRecord,
    wait_id: &str,
) {
    seed_live_work_execution(
        runtime,
        work_item,
        crate::domain::execution_protocol::WorkItemExecutionState::Waiting {
            generation: work_item.revision.max(1),
            wait: crate::domain::execution_protocol::WaitReference {
                wait_id: wait_id.to_owned(),
            },
        },
    )
    .await;
}

pub(crate) async fn seed_live_work_execution(
    runtime: &RuntimeHandle,
    work_item: &WorkItemRecord,
    state: crate::domain::execution_protocol::WorkItemExecutionState,
) {
    use crate::domain::execution_protocol::WorkItemExecutionRecord;
    use crate::runtime_db::input_evidence_snapshot::CanonicalInputEvidence;
    use crate::runtime_db::input_evidence_writes::acknowledge_input_evidence_tx;
    use crate::runtime_db::transitions::execution_protocol_repository::{
        load_state_unchecked_tx, persist_repaired_state_tx,
    };

    let db = runtime.inner.runtime_db.clone();
    let agent_id = work_item.agent_id.clone();
    let work_item_id = work_item.id.clone();
    let source_revision = work_item.revision;
    runtime
        .commit_input_evidence(crate::ids::audit_event_id(), move |scope| {
            db.transaction(|tx| {
                let previous = load_state_unchecked_tx(tx, &agent_id)?;
                let mut execution = previous.clone();
                execution.work_items.insert(
                    work_item_id.clone(),
                    WorkItemExecutionRecord {
                        source_revision,
                        state: state.clone(),
                    },
                );
                persist_repaired_state_tx(tx, &previous, &execution)?;
                let actual: WorkItemExecutionRecord = serde_json::from_str(&tx.query_row(
                    "SELECT payload_json FROM execution_protocol_work_items
                     WHERE agent_id=?1 AND work_item_id=?2",
                    rusqlite::params![agent_id, work_item_id],
                    |row| row.get::<_, String>(0),
                )?)?;
                acknowledge_input_evidence_tx(
                    tx,
                    scope,
                    (),
                    &[CanonicalInputEvidence::WorkExecution {
                        work_item_id: work_item_id.clone(),
                        record: actual,
                    }],
                )
            })
        })
        .await
        .unwrap();
}

fn seed_work_execution(
    storage: &AppStorage,
    work_item: &WorkItemRecord,
    state: crate::domain::execution_protocol::WorkItemExecutionState,
) {
    let runtime_db = storage
        .runtime_db()
        .unwrap()
        .expect("test storage should expose RuntimeDb");
    let mut execution = runtime_db
        .transitions()
        .load_execution_protocol_state_if_initialized(&work_item.agent_id)
        .unwrap()
        .unwrap_or_else(|| {
            crate::domain::execution_protocol::ExecutionProtocolState::empty(&work_item.agent_id)
        });
    execution.work_items.insert(
        work_item.id.clone(),
        crate::domain::execution_protocol::WorkItemExecutionRecord {
            source_revision: work_item.revision,
            state,
        },
    );
    runtime_db
        .transaction(|tx| crate::runtime_db::transitions::persist_state_tx(tx, &execution))
        .unwrap();
}

pub(crate) async fn seed_bound_work_item(
    runtime: &RuntimeHandle,
    state: WorkItemState,
    summary: Option<&str>,
    blocked_by: Option<&str>,
) -> String {
    let mut record = runtime
        .create_work_item(
            summary.unwrap_or("finish the bound objective").to_string(),
            None,
            None,
            Vec::new(),
        )
        .await
        .unwrap();
    if let Some(blocked_by) = blocked_by {
        record = runtime
            .update_work_item_fields(
                record.id.clone(),
                None,
                None,
                None,
                None,
                Some(Some(blocked_by.to_string())),
            )
            .await
            .unwrap();
    }
    if state == WorkItemState::Completed {
        record = runtime
            .complete_work_item(record.id.clone(), Vec::new())
            .await
            .unwrap();
        if let Some(summary) = summary {
            record = runtime
                .promote_work_item_completion_report(
                    record.id.clone(),
                    summary.to_string(),
                    None,
                    None,
                    Vec::new(),
                )
                .await
                .unwrap();
        }
    }
    bind_turn_to_work_item(runtime, &record.id).await;
    record.id
}

pub(crate) async fn mark_blocking_task(runtime: &RuntimeHandle, task_id: &str) {
    runtime
        .persist_task_transition(
            &TaskRecord {
                id: task_id.into(),
                agent_id: "default".into(),
                kind: TaskKind::CommandTask,
                status: TaskStatus::Running,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                parent_message_id: None,
                work_item_id: None,
                summary: Some("blocking command".into()),
                detail: Some(serde_json::json!({
                    "wait_policy": "blocking"
                })),
                recovery: None,
            },
            "blocking_task_fixture",
        )
        .await
        .unwrap();
}

pub(crate) struct TruncatingProvider {
    pub(crate) calls: Mutex<usize>,
}

impl TruncatingProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct EmptyTruncatingProvider;

pub(crate) struct TimelineProvider;

pub(crate) struct OneToolThenTextProvider {
    pub(crate) calls: Mutex<usize>,
}

impl OneToolThenTextProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

#[async_trait]
impl AgentProvider for OneToolThenTextProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            vec![ModelBlock::ToolUse {
                id: "verify".into(),
                name: "ExecCommand".into(),
                input: serde_json::json!({
                    "cmd": "printf 'ok'",
                    "shell": "sh"
                }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }]
        } else {
            vec![ModelBlock::Text {
                text: "done".into(),
            }]
        };
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

pub(crate) struct FailingTimelineProvider;

pub(crate) struct ToolCaptureProvider {
    pub(crate) requests: Mutex<Vec<Vec<String>>>,
}

impl ToolCaptureProvider {
    pub(crate) async fn request_history(&self) -> tokio::sync::MutexGuard<'_, Vec<Vec<String>>> {
        self.requests.lock().await
    }
}

pub(crate) struct TurnLocalCompactionProbeProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) requests: Mutex<Vec<ProviderTurnRequest>>,
}

impl TurnLocalCompactionProbeProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
    pub(crate) async fn request_history(
        &self,
    ) -> tokio::sync::MutexGuard<'_, Vec<ProviderTurnRequest>> {
        self.requests.lock().await
    }
}

pub(crate) struct BaselineOverBudgetProbeProvider {
    pub(crate) calls: Mutex<usize>,
}

impl BaselineOverBudgetProbeProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct LargeBudgetContinuationProbeProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct RecentTurnsRecoveryProbeProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) requests: Mutex<Vec<ProviderTurnRequest>>,
}

pub(crate) struct ContextLengthExceededProvider;

pub(crate) struct RecoveringContextLengthProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct DeferredFallbackProvider;

pub(crate) struct DeferredNetworkFallbackProvider;

pub(crate) struct TextThenFailingFallbackProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct SleepOnlyToolProvider {
    pub(crate) calls: Mutex<usize>,
}

impl SleepOnlyToolProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct WaitForOnlyToolProvider {
    pub(crate) calls: Mutex<usize>,
}

impl WaitForOnlyToolProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct WaitForFinalReportProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) scenario: WaitForFinalReportScenario,
    pub(crate) silent_progress: Option<bool>,
    pub(crate) report_path: std::path::PathBuf,
    pub(crate) report_runtime: Mutex<Option<RuntimeHandle>>,
    pub(crate) saw_settlement_error_follow_up: Mutex<bool>,
    pub(crate) settlement_result_count: Mutex<usize>,
    pub(crate) settlement_timer: Mutex<Option<(RuntimeHandle, String)>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReportCheckpointOutput {
    Text,
    Empty,
    ReasoningOnly,
}

const REPORT_COMPACTION_TRIGGER_ESTIMATED_TOKENS: usize = 28000;

pub(crate) struct ReportCompactionProvider {
    pub(crate) inner: Arc<dyn AgentProvider>,
    pub(crate) checkpoint_output: ReportCheckpointOutput,
    pub(crate) checkpoint_calls: Mutex<usize>,
    pub(crate) response_calls: Mutex<usize>,
}

#[async_trait]
impl AgentProvider for ReportCompactionProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut result_ids = std::collections::HashSet::new();
        for (index, message) in request.conversation.iter().enumerate() {
            if let ConversationMessage::UserToolResults(results) = message {
                let Some(ConversationMessage::AssistantBlocks(blocks)) = index
                    .checked_sub(1)
                    .and_then(|i| request.conversation.get(i))
                else {
                    panic!("checkpoint projection must preserve adjacent tool uses and results");
                };
                for result in results {
                    assert!(blocks.iter().any(|block| matches!(
                        block, ModelBlock::ToolUse { id, .. } if id == &result.tool_use_id
                    )));
                    assert!(result_ids.insert(&result.tool_use_id));
                }
            }
        }
        if request.conversation.iter().any(|message| {
            matches!(
                message,
                ConversationMessage::UserText(text) if text.contains("progress checkpoint request")
            )
        }) {
            *self.checkpoint_calls.lock().await += 1;
            return Ok(ProviderTurnResponse {
                material: request.material,
                blocks: match self.checkpoint_output {
                    ReportCheckpointOutput::Text => vec![ModelBlock::Text {
                        text:
                            "Private checkpoint: cleanup remains; deliver a separate final report."
                                .into(),
                    }],
                    ReportCheckpointOutput::Empty => Vec::new(),
                    ReportCheckpointOutput::ReasoningOnly => vec![ModelBlock::ReasoningText {
                        text: "Private reasoning about unfinished cleanup.".into(),
                    }],
                },
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 10,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            });
        }
        let mut calls = self.response_calls.lock().await;
        *calls += 1;
        let mut response = self.inner.complete_turn(request).await?;
        // Text alongside the initial lifecycle call would satisfy its report
        // immediately, without entering the pending-report continuation.
        if *calls == 2 {
            // Trigger compaction independently of machine-local guidance.
            let mut text = String::new();
            while crate::token_estimate::estimate_text_tokens(&text)
                <= REPORT_COMPACTION_TRIGGER_ESTIMATED_TOKENS
            {
                text.push_str("Pending report context that may be compacted. ");
            }
            response.blocks.insert(0, ModelBlock::Text { text });
        }
        Ok(response)
    }
}

pub(crate) fn report_compaction_context_config(workspace: &TempDir) -> ContextConfig {
    let config = continuation_ready_context_config(workspace, 65536);
    ContextConfig {
        // Leave room for the ordinary runtime prompt and the checkpoint request,
        // but not for the long pending-report round as well.
        compaction_trigger_estimated_tokens: config.prompt_budget_estimated_tokens - 65536
            + REPORT_COMPACTION_TRIGGER_ESTIMATED_TOKENS,
        compaction_keep_recent_estimated_tokens: 2048,
        ..config
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WaitForFinalReportScenario {
    Direct,
    NestedLifecycleCorrective,
    CleanupTools,
    SettlementErrorRecovery,
}

impl WaitForFinalReportProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct AbandonWaitForFinalReportProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct DisallowedToolThenTextProvider {
    pub(crate) calls: Mutex<usize>,
}

impl DisallowedToolThenTextProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

pub(crate) struct MaxOutputMutationToolProvider {
    pub(crate) calls: Mutex<usize>,
}

impl MaxOutputMutationToolProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

#[async_trait]
impl AgentProvider for TruncatingProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![
                    ModelBlock::Text {
                        text: "Partial report heading:".into(),
                    },
                    ModelBlock::Citations {
                        citations: vec![crate::types::Citation {
                            url: "https://example.com/first".into(),
                            title: Some("First".into()),
                        }],
                    },
                ],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 100,
                output_tokens: 50,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            });
        }

        assert!(request.conversation.iter().any(|message| match message {
            ConversationMessage::UserText(text) => {
                text.contains("Output token limit hit")
                    && text.contains("Continue exactly where you left off")
                    && text.contains("Do not restart from the top, repeat analysis")
                    && text.contains("re-read context already provided")
            }
            _ => false,
        }));

        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![
                ModelBlock::Text {
                    text: "\n\n- final grounded recommendation".into(),
                },
                ModelBlock::Citations {
                    citations: vec![
                        crate::types::Citation {
                            url: "https://example.com/first".into(),
                            title: Some("Duplicate".into()),
                        },
                        crate::types::Citation {
                            url: "https://example.com/second".into(),
                            title: Some("Second".into()),
                        },
                    ],
                },
            ],
            stop_reason: None,
            input_tokens: 50,
            output_tokens: 25,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for EmptyTruncatingProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: Vec::new(),
            stop_reason: Some("max_tokens".into()),
            input_tokens: 100,
            output_tokens: 50,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for TimelineProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: "done with fallback history".into(),
            }],
            stop_reason: None,
            input_tokens: 12,
            output_tokens: 6,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    async fn complete_turn_with_diagnostics(
        &self,
        request: ProviderTurnRequest,
    ) -> Result<(ProviderTurnResponse, Option<ProviderAttemptTimeline>)> {
        let response = self.complete_turn(request).await?;
        Ok((
            response,
            Some(ProviderAttemptTimeline {
                attempts: vec![
                    ProviderAttemptRecord {
                        provider: "openai".into(),
                        model_ref: "openai/gpt-5.4".into(),
                        attempt: 1,
                        max_attempts: 3,
                        started_at: None,
                        completed_at: None,
                        duration_ms: Some(125),
                        failure_kind: Some("server_error".into()),
                        disposition: Some("retryable".into()),
                        outcome: ProviderAttemptOutcome::Retrying,
                        advanced_to_fallback: false,
                        backoff_ms: Some(200),
                        backoff_source: None,
                        token_usage: None,
                        cache_usage: None,
                        provider_message_id: None,
                        provider_request_id: None,
                        provider_http_trace_id: None,
                        transport_timeline: None,
                        transport_diagnostics: None,
                    },
                    ProviderAttemptRecord {
                        provider: "anthropic".into(),
                        model_ref: "anthropic/claude-sonnet-4-6".into(),
                        attempt: 1,
                        max_attempts: 3,
                        started_at: None,
                        completed_at: None,
                        duration_ms: Some(250),
                        failure_kind: None,
                        disposition: None,
                        outcome: ProviderAttemptOutcome::Succeeded,
                        advanced_to_fallback: false,
                        backoff_ms: None,
                        backoff_source: None,
                        token_usage: Some(TokenUsage::new(12, 6)),
                        cache_usage: None,
                        provider_message_id: None,
                        provider_request_id: None,
                        provider_http_trace_id: None,
                        transport_timeline: None,
                        transport_diagnostics: None,
                    },
                ],
                aggregated_token_usage: Some(TokenUsage::new(12, 6)),
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                winning_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                pending_fallback_model_ref: None,
                pending_fallback_disposition: None,
            }),
        ))
    }
}

#[async_trait]
impl AgentProvider for ToolCaptureProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(
            request
                .tools
                .iter()
                .map(|tool| tool.name.clone())
                .collect::<Vec<_>>(),
        );
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: "captured tool set".into(),
            }],
            stop_reason: None,
            input_tokens: 8,
            output_tokens: 4,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for TurnLocalCompactionProbeProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(request);
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let response = match *calls {
            1 => ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![
                    ModelBlock::Text {
                        text: format!("Round 1 planning {}", "very detailed preamble ".repeat(150)),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-1".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'first-round-output-should-not-stay-exact'",
                            "yield_time_ms": 30000
                        }),
                        kind: crate::provider::ModelToolCallKind::Function,
                        provider_data: None,
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            },
            2 => ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 2 planning keep recent exact.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-2".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'second-round-output-should-remain-exact'",
                            "yield_time_ms": 30000
                        }),
                        kind: crate::provider::ModelToolCallKind::Function,
                        provider_data: None,
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            },
            3 => ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![
                    ModelBlock::Text {
                        text: "Round 3 planning keep recent exact too.".into(),
                    },
                    ModelBlock::ToolUse {
                        id: "exec-round-3".into(),
                        name: "ExecCommand".into(),
                        input: serde_json::json!({
                            "cmd": "printf 'third-round-output-should-remain-exact'",
                            "yield_time_ms": 30000
                        }),
                        kind: crate::provider::ModelToolCallKind::Function,
                        provider_data: None,
                    },
                ],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            },
            5 => ProviderTurnResponse {
                material: Default::default(),
                blocks: Vec::new(),
                stop_reason: Some("end_turn".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            },
            _ => ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::Text {
                    text: "Finished after compacted continuation.".into(),
                }],
                stop_reason: Some("end_turn".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            },
        };
        Ok(response)
    }
}

#[async_trait]
impl AgentProvider for BaselineOverBudgetProbeProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-baseline-over-budget".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'baseline-over-budget'"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                }],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            _ => panic!("continuation request should not be sent after baseline-over-budget"),
        }
    }
}

#[async_trait]
impl AgentProvider for LargeBudgetContinuationProbeProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-large-budget-continuation".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'large-budget-continuation'"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                }],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            2 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::Text {
                    text: "Finished within the resolved model prompt budget.".into(),
                }],
                stop_reason: Some("end_turn".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            _ => panic!("large-budget continuation should finish on the second provider call"),
        }
    }
}

#[async_trait]
impl AgentProvider for SleepOnlyToolProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls > 1 {
            anyhow::bail!("sleep-only round should not force another provider turn");
        }
        assert!(
            request.tools.iter().all(|tool| tool.name != "Sleep"),
            "Sleep must stay hidden from the provider tool surface"
        );

        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::ToolUse {
                id: "sleep-1".into(),
                name: "Sleep".into(),
                input: serde_json::json!({
                    "reason": "waiting for review",
                    "duration_ms": 250
                }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for WaitForOnlyToolProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls > 1 {
            anyhow::bail!("wait-for-only round should not force another provider turn");
        }
        assert!(
            request.tools.iter().any(|tool| tool.name == "WaitFor"),
            "WaitFor must be exposed to the provider tool surface"
        );

        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::ToolUse {
                id: "wait-for-1".into(),
                name: "WaitFor".into(),
                input: serde_json::json!({
                    "reason": "waiting for PR checks",
                    "wake": "external",
                    "delivery": "silent",
                    "resource": "github:holon-run/holon#1939",
                    "recheck_after_ms": 1800000
                }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for WaitForFinalReportProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls > 1
            && self.silent_progress.is_none()
            && matches!(
                self.scenario,
                WaitForFinalReportScenario::CleanupTools
                    | WaitForFinalReportScenario::NestedLifecycleCorrective
            )
        {
            for name in ["ExecCommand", "ApplyPatch", "CreateWorkItem", "GetAgent"] {
                assert!(
                    request.tools.iter().any(|tool| tool.name == name),
                    "ordinary tool {name} must remain available during the wait report"
                );
            }
            for name in ["WaitFor", "CompleteWorkItem", "PickWorkItem", "Sleep"] {
                assert!(
                    request.tools.iter().all(|tool| tool.name != name),
                    "nested lifecycle tool {name} must not be exposed"
                );
            }
            let runtime = self.report_runtime.lock().await.clone().unwrap();
            assert!(
                runtime
                    .storage()
                    .active_wait_conditions_for_agent("default")?
                    .is_empty(),
                "text accompanying cleanup tools must not register the wait"
            );
            assert!(runtime
                .recent_briefs(10)
                .await?
                .iter()
                .all(|brief| brief.kind != BriefKind::Result));
            if self.scenario == WaitForFinalReportScenario::CleanupTools && *calls > 2 {
                let mut previous_ids = vec![format!("cleanup-wait-tool-{}", *calls - 1)];
                if *calls == 3 {
                    previous_ids.push("cleanup-wait-batch-check".into());
                }
                for previous_id in previous_ids {
                    let results = request
                        .conversation
                        .iter()
                        .filter_map(|message| match message {
                            ConversationMessage::UserToolResults(results) => Some(results),
                            _ => None,
                        })
                        .flatten()
                        .filter(|result| result.tool_use_id == previous_id)
                        .collect::<Vec<_>>();
                    assert_eq!(
                        results.len(),
                        1,
                        "cleanup tool {previous_id} must have exactly one result"
                    );
                    assert!(!results[0].is_error, "{}", results[0].content);
                }
            }
            if self.scenario == WaitForFinalReportScenario::NestedLifecycleCorrective && *calls == 3
            {
                assert!(request.conversation.iter().all(|message| match message {
                    ConversationMessage::AssistantBlocks(blocks) => blocks.iter().all(|block| {
                        !matches!(block, ModelBlock::ToolUse { id, .. } if id.starts_with("nested-"))
                    }),
                    _ => true,
                }));
            }
        }
        let blocks = if let Some(prior_round) = self.silent_progress {
            let progress_round = prior_round && *calls == 1;
            assert!(*calls <= if prior_round { 2 } else { 1 });
            let mut blocks = Vec::new();
            if !prior_round || progress_round {
                blocks.push(ModelBlock::Text {
                    text: "This is progress, not a final report.".into(),
                });
            }
            blocks.push(ModelBlock::ToolUse {
                id: format!("silent-wait-{}", *calls),
                name: if progress_round {
                    "GetAgent"
                } else {
                    "WaitFor"
                }
                .into(),
                input: if progress_round {
                    serde_json::json!({})
                } else {
                    serde_json::json!({
                        "reason": "wait without publishing progress as a brief",
                        "wake": "external",
                        "delivery": "silent",
                        "resource": "github:holon-run/holon#wait-silent"
                    })
                },
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            });
            blocks
        } else {
            match *calls {
                1 => {
                    let settlement_timer = self.settlement_timer.lock().await;
                    let (id, resource) =
                        if self.scenario == WaitForFinalReportScenario::SettlementErrorRecovery {
                            (
                                "wait-for-invalid-final",
                                settlement_timer.as_ref().unwrap().1.as_str(),
                            )
                        } else {
                            ("wait-for-final", "github:holon-run/holon#wait-final")
                        };
                    vec![ModelBlock::ToolUse {
                        id: id.into(),
                        name: "WaitFor".into(),
                        input: serde_json::json!({
                            "reason": "waiting for final verification",
                            "wake": if self.scenario == WaitForFinalReportScenario::SettlementErrorRecovery {
                                "timer"
                            } else {
                                "external"
                            },
                            "delivery": "final",
                            "resource": resource
                        }),
                        kind: crate::provider::ModelToolCallKind::Function,
                        provider_data: None,
                    }]
                }
                2 if self.scenario == WaitForFinalReportScenario::SettlementErrorRecovery => {
                    let (runtime, timer_id) = self.settlement_timer.lock().await.take().unwrap();
                    runtime.cancel_timer(&timer_id).await?;
                    vec![ModelBlock::Text {
                        text: "The requested timer wait could not be completed.".into(),
                    }]
                }
                3 if self.scenario == WaitForFinalReportScenario::SettlementErrorRecovery => {
                    let follow_up = request.conversation.iter().any(|message| {
                        matches!(
                            message,
                            ConversationMessage::UserText(text)
                                if text.contains("recoverable tool error")
                        )
                    });
                    let result_count = request
                        .conversation
                        .iter()
                        .filter_map(|message| match message {
                            ConversationMessage::UserToolResults(results) => Some(results),
                            _ => None,
                        })
                        .flatten()
                        .filter(|result| result.tool_use_id == "wait-for-invalid-final")
                        .count();
                    *self.saw_settlement_error_follow_up.lock().await = follow_up;
                    *self.settlement_result_count.lock().await = result_count;
                    vec![ModelBlock::Text {
                        text: "The timer wait failed and no wait was registered.".into(),
                    }]
                }
                2 if self.scenario == WaitForFinalReportScenario::NestedLifecycleCorrective => {
                    vec![
                        ModelBlock::Text {
                            text: "This text must not commit a recursive wait.".into(),
                        },
                        ModelBlock::ToolUse {
                            id: "nested-wait".into(),
                            name: "WaitFor".into(),
                            input: serde_json::json!({
                                "wake": "operator_input",
                                "reason": "must not replace pending wait",
                                "delivery": "silent"
                            }),
                            kind: crate::provider::ModelToolCallKind::Function,
                            provider_data: None,
                        },
                        ModelBlock::ToolUse {
                            id: "nested-completion".into(),
                            name: "CompleteWorkItem".into(),
                            input: serde_json::json!({"work_item_id": "must-not-complete"}),
                            kind: crate::provider::ModelToolCallKind::Function,
                            provider_data: None,
                        },
                    ]
                }
                2..=6 if self.scenario == WaitForFinalReportScenario::CleanupTools => {
                    let path = self.report_path.display();
                    let (name, input) = match *calls {
                        2 => (
                            "ExecCommand",
                            serde_json::json!({
                                "cmd": format!("printf 'before\\n' > '{path}'"),
                                "yield_time_ms": 30000
                            }),
                        ),
                        3 => {
                            assert_eq!(std::fs::read_to_string(&self.report_path)?, "before\n");
                            let patch_tool = request
                                .tools
                                .iter()
                                .find(|tool| tool.name == "ApplyPatch")
                                .unwrap();
                            (
                                "ApplyPatch",
                                if patch_tool.freeform_grammar.is_some() {
                                    serde_json::json!(format!(
                                        "*** Begin Patch\n*** Update File: {path}\n@@\n-before\n+after\n*** End Patch\n"
                                    ))
                                } else {
                                    serde_json::json!({
                                        "patch": format!("--- {path}\n+++ {path}\n@@ -1,1 +1,1 @@\n-before\n+after\n")
                                    })
                                },
                            )
                        }
                        4 | 5 => {
                            assert_eq!(std::fs::read_to_string(&self.report_path)?, "after\n");
                            (
                                "ExecCommand",
                                serde_json::json!({
                                    "cmd": if *calls == 4 {
                                        format!("cat '{path}'")
                                    } else {
                                        format!("rm '{path}'")
                                    },
                                    "yield_time_ms": 30000
                                }),
                            )
                        }
                        6 => {
                            assert!(!self.report_path.exists());
                            (
                                "ExecCommand",
                                serde_json::json!({
                                    "cmd": format!("test ! -e '{path}'"),
                                    "yield_time_ms": 30000
                                }),
                            )
                        }
                        _ => unreachable!(),
                    };
                    let mut blocks = vec![
                        ModelBlock::Text {
                            text: "Cleanup is in progress; this is not the final wait report."
                                .into(),
                        },
                        ModelBlock::ToolUse {
                            id: format!("cleanup-wait-tool-{}", *calls),
                            name: name.into(),
                            kind: if name == "ApplyPatch" && input.is_string() {
                                crate::provider::ModelToolCallKind::Custom
                            } else {
                                crate::provider::ModelToolCallKind::Function
                            },
                            input,
                            provider_data: None,
                        },
                    ];
                    if *calls == 2 {
                        blocks.push(ModelBlock::ToolUse {
                            id: "cleanup-wait-batch-check".into(),
                            name: "ExecCommand".into(),
                            input: serde_json::json!({
                                "cmd": format!("test -s '{path}'"),
                                "yield_time_ms": 30000
                            }),
                            kind: crate::provider::ModelToolCallKind::Function,
                            provider_data: None,
                        });
                    }
                    blocks
                }
                7 if self.scenario == WaitForFinalReportScenario::CleanupTools => {
                    assert!(!self.report_path.exists());
                    vec![ModelBlock::Text {
                        text: "Waiting for final verification; I will resume when it changes."
                            .into(),
                    }]
                }
                2 | 3 => vec![ModelBlock::Text {
                    text: "Waiting for final verification; I will resume when it changes.".into(),
                }],
                _ => anyhow::bail!("unexpected WaitFor final report provider call"),
            }
        };
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 10,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for AbandonWaitForFinalReportProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        let blocks = if *calls == 1 {
            vec![ModelBlock::ToolUse {
                id: "wait-for-abandoned-final".into(),
                name: "WaitFor".into(),
                input: serde_json::json!({
                    "reason": "waiting for abandoned final report",
                    "wake": "external",
                    "delivery": "final",
                    "resource": "github:holon-run/holon#wait-abandoned"
                }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }]
        } else {
            Vec::new()
        };
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 1,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for DisallowedToolThenTextProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::ToolUse {
                    id: "legacy-task".into(),
                    name: "CreateTask".into(),
                    input: serde_json::json!({
                        "prompt": "removed public task surface"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                }],
                stop_reason: None,
                input_tokens: 10,
                output_tokens: 5,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            2 => {
                assert!(
                    request.conversation.iter().any(|message| matches!(
                        message,
                        ConversationMessage::UserToolResults(results)
                            if results.iter().any(|result|
                                result.tool_use_id == "legacy-task"
                                    && result.is_error
                                    && result
                                        .error
                                        .as_ref()
                                        .is_some_and(|error| error.kind == "tool_not_exposed_for_round")
                            )
                    )),
                    "continuation should receive a structured error for the unavailable tool"
                );
                Ok(ProviderTurnResponse {
                    material: Default::default(),
                    blocks: vec![ModelBlock::Text {
                        text: "Recovered after unavailable tool.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 10,
                    output_tokens: 5,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    request_diagnostics: None,
                })
            }
            _ => anyhow::bail!("unexpected provider call after recovery text"),
        }
    }
}

#[async_trait]
impl AgentProvider for MaxOutputMutationToolProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::ToolUse {
                    id: "truncated-patch".into(),
                    name: "ApplyPatch".into(),
                    input: serde_json::json!({
                        "patch": "--- /dev/null\n+++ b/app.txt\n@@ -0,0 +1 @@\n+should-not-be-written\n"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                }],
                stop_reason: Some("max_tokens".into()),
                input_tokens: 20,
                output_tokens: 10,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            2 => {
                assert!(
                    request.conversation.iter().any(|message| matches!(
                        message,
                        ConversationMessage::UserToolResults(results)
                            if results.iter().any(|result|
                                result.tool_use_id == "truncated-patch"
                                    && result.is_error
                                    && result
                                        .error
                                        .as_ref()
                                        .is_some_and(|error| error.kind == "truncated_mutation_tool_call")
                            )
                    )),
                    "continuation should receive a structured truncation error"
                );
                Ok(ProviderTurnResponse {
                    material: Default::default(),
                    blocks: vec![ModelBlock::Text {
                        text: "Recovered after rejected truncated mutation.".into(),
                    }],
                    stop_reason: None,
                    input_tokens: 15,
                    output_tokens: 8,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    request_diagnostics: None,
                })
            }
            _ => panic!("provider should stop after recovery"),
        }
    }
}

#[async_trait]
impl AgentProvider for FailingTimelineProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "all configured providers failed for this turn: openai/gpt-5.4: fail_fast (contract_error): bad request",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 1,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("contract_error".into()),
                    disposition: Some("fail_fast".into()),
                    outcome: ProviderAttemptOutcome::FailFastAborted,
                    advanced_to_fallback: false,
                    backoff_ms: None,
                    backoff_source: None,
                    token_usage: None,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    provider_http_trace_id: None,
                    transport_timeline: None,
                    transport_diagnostics: Some(ProviderTransportDiagnostics {
                        stage: "request_send".into(),
                        streaming: None,
                        provider: Some("openai".into()),
                        model_ref: Some("openai/gpt-5.4".into()),
                        url: Some(
                            "https://user:secret@example.com/v1/responses?api_key=token#frag"
                                .into(),
                        ),
                        status: None,
                        reqwest: Some(ReqwestTransportDiagnostics {
                            is_timeout: false,
                            is_connect: false,
                            is_request: false,
                            is_body: true,
                            is_decode: false,
                            is_redirect: false,
                            status: None
                        }),
                        context_budget: None,
                        http_trace: Some(ProviderHttpTraceDiagnostics {
                            capture_id: "trace-1-1".into(),
                            mode: "failure_only".into(),
                            path: "/Users/example/.holon/http-trace/default/trace-1-1.jsonl"
                                .into(),
                            status: None
                        }),
                        quota_identity: None,
                        retry_after_not_before: None,
                        throttle: None,
                        source_chain: vec!["connection reset by peer".into()]
                    })
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: None,
                pending_fallback_disposition: None,
            },
            anyhow!("bad request"),
        ))
    }
}

#[async_trait]
impl AgentProvider for RecentTurnsRecoveryProbeProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        self.requests.lock().await.push(request);
        let mut calls = self.calls.lock().await;
        *calls += 1;
        match *calls {
            1 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::ToolUse {
                    id: "exec-recent-turns-recovery".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'recent-turns-recovery:'; awk 'BEGIN { for (i = 0; i < 1000; i++) printf \"x \" }'"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                }],
                stop_reason: Some("tool_use".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            2 => Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::Text {
                    text: "Recovered by shrinking recent turns.".into(),
                }],
                stop_reason: Some("end_turn".into()),
                input_tokens: 0,
                output_tokens: 0,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            }),
            _ => panic!("recent-turns recovery should require exactly one continuation"),
        }
    }
}

#[async_trait]
impl AgentProvider for ContextLengthExceededProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "all configured providers failed for this turn: openai-codex/gpt-5.3-codex-spark: fail_fast (contract_error): context_length_exceeded",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai-codex".into(),
                    model_ref: "openai-codex/gpt-5.3-codex-spark".into(),
                    attempt: 1,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("contract_error".into()),
                    disposition: Some("fail_fast".into()),
                    outcome: ProviderAttemptOutcome::FailFastAborted,
                    advanced_to_fallback: false,
                    backoff_ms: None,
                    backoff_source: None,
                    token_usage: Some(TokenUsage::new(125_166, 0)),
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    provider_http_trace_id: None,
                    transport_timeline: None,
                    transport_diagnostics: None
                }],
                aggregated_token_usage: Some(TokenUsage::new(125_166, 0)),
                requested_model_ref: "openai-codex/gpt-5.3-codex-spark".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: None,
                pending_fallback_disposition: None,
            },
            provider_transport_error_with_code(
                ProviderFailureClassification {
                    kind: ProviderFailureKind::ContractError,
                    disposition: RetryDisposition::FailFast,
                },
                Some("context_length_exceeded"),
                None,
                None,
                "context_length_exceeded: input too long",
            ),
        ))
    }
}

#[async_trait]
impl AgentProvider for RecoveringContextLengthProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            let now = Utc::now();
            return Err(provider_turn_error(
                "context_length_exceeded",
                ProviderAttemptTimeline {
                    attempts: vec![ProviderAttemptRecord {
                        provider: "test".into(),
                        model_ref: "test/model".into(),
                        attempt: 1,
                        max_attempts: 1,
                        started_at: Some(now),
                        completed_at: Some(now),
                        duration_ms: Some(1),
                        failure_kind: Some("context_length_exceeded".into()),
                        disposition: Some("fail_fast".into()),
                        outcome: ProviderAttemptOutcome::FailFastAborted,
                        advanced_to_fallback: false,
                        backoff_ms: None,
                        backoff_source: None,
                        token_usage: None,
                        cache_usage: None,
                        provider_message_id: None,
                        provider_request_id: None,
                        provider_http_trace_id: None,
                        transport_timeline: Some(ProviderTransportTimeline {
                            request_started_at: now,
                            response_headers_at: now,
                            response_body_completed_at: Some(now),
                            parse_completed_at: now,
                            streaming: false,
                        }),
                        transport_diagnostics: None,
                    }],
                    requested_model_ref: "test/model".into(),
                    active_model_ref: None,
                    winning_model_ref: None,
                    pending_fallback_model_ref: None,
                    pending_fallback_disposition: None,
                    aggregated_token_usage: None,
                },
                provider_transport_error_with_code(
                    ProviderFailureClassification {
                        kind: ProviderFailureKind::ContractError,
                        disposition: RetryDisposition::FailFast,
                    },
                    Some("context_length_exceeded"),
                    Some(400),
                    None,
                    "context_length_exceeded",
                ),
            ));
        }
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: "Recovered after reducing recent turns.".into(),
            }],
            stop_reason: Some("end_turn".into()),
            input_tokens: 1,
            output_tokens: 1,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for DeferredFallbackProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "current provider failed for this turn: openai/gpt-5.4: retryable exhausted",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 3,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("server_error".into()),
                    disposition: Some("retryable".into()),
                    outcome: ProviderAttemptOutcome::RetriesExhausted,
                    advanced_to_fallback: true,
                    backoff_ms: None,
                    backoff_source: None,
                    token_usage: None,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    provider_http_trace_id: None,
                    transport_timeline: None,
                    transport_diagnostics: None,
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                pending_fallback_disposition: Some(
                    crate::provider::ProviderFallbackDisposition::Immediate,
                ),
            },
            anyhow!("server unavailable"),
        ))
    }
}

#[async_trait]
impl AgentProvider for DeferredNetworkFallbackProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        Err(provider_turn_error(
            "current provider failed for this turn: openai/gpt-5.4: retryable exhausted",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 3,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("connection".into()),
                    disposition: Some("retryable".into()),
                    outcome: ProviderAttemptOutcome::RetriesExhausted,
                    advanced_to_fallback: true,
                    backoff_ms: None,
                    backoff_source: None,
                    token_usage: None,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    provider_http_trace_id: None,
                    transport_diagnostics: None,
                    transport_timeline: None,
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                pending_fallback_disposition: Some(
                    crate::provider::ProviderFallbackDisposition::Deferred,
                ),
            },
            anyhow!("network unavailable"),
        ))
    }
}

#[async_trait]
impl AgentProvider for TextThenFailingFallbackProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        if *calls == 1 {
            return Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::Text {
                    text: "Partial report heading".into(),
                }],
                stop_reason: Some("max_output_tokens".into()),
                input_tokens: 20,
                output_tokens: 10,
                cache_usage: None,
                provider_message_id: None,
                provider_request_id: None,
                request_diagnostics: None,
            });
        }

        Err(provider_turn_error(
            "current provider failed for this turn: openai/gpt-5.4: retryable exhausted",
            ProviderAttemptTimeline {
                attempts: vec![ProviderAttemptRecord {
                    provider: "openai".into(),
                    model_ref: "openai/gpt-5.4".into(),
                    attempt: 3,
                    max_attempts: 3,
                    started_at: None,
                    completed_at: None,
                    duration_ms: Some(125),
                    failure_kind: Some("server_error".into()),
                    disposition: Some("retryable".into()),
                    outcome: ProviderAttemptOutcome::RetriesExhausted,
                    advanced_to_fallback: true,
                    backoff_ms: None,
                    backoff_source: None,
                    token_usage: None,
                    cache_usage: None,
                    provider_message_id: None,
                    provider_request_id: None,
                    provider_http_trace_id: None,
                    transport_timeline: None,
                    transport_diagnostics: None,
                }],
                aggregated_token_usage: None,
                requested_model_ref: "openai/gpt-5.4".into(),
                active_model_ref: None,
                winning_model_ref: None,
                pending_fallback_model_ref: Some("anthropic/claude-sonnet-4-6".into()),
                pending_fallback_disposition: Some(
                    crate::provider::ProviderFallbackDisposition::Immediate,
                ),
            },
            anyhow!("server unavailable"),
        ))
    }
}

pub(crate) struct StagnatingAfterVerificationProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct SkillReadProvider {
    pub(crate) calls: Mutex<usize>,
}

pub(crate) struct SkillActivationCommandProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) tool_name: &'static str,
    pub(crate) input: serde_json::Value,
}

pub(crate) struct CountingProvider {
    pub(crate) calls: Mutex<usize>,
    pub(crate) reply: &'static str,
}

impl StagnatingAfterVerificationProvider {
    pub(crate) fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

impl SkillReadProvider {
    pub(crate) fn new() -> Self {
        Self {
            calls: Mutex::new(0),
        }
    }
}

impl SkillActivationCommandProvider {
    pub(crate) fn new(tool_name: &'static str, input: serde_json::Value) -> Self {
        Self {
            calls: Mutex::new(0),
            tool_name,
            input,
        }
    }
}

impl CountingProvider {
    pub(crate) async fn call_count(&self) -> usize {
        *self.calls.lock().await
    }
}

#[async_trait]
impl AgentProvider for StagnatingAfterVerificationProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        if request.tools.is_empty() {
            return Ok(ProviderTurnResponse {
                material: Default::default(),
                blocks: vec![ModelBlock::Text {
                    text: "What changed: app.txt\nWhy: to address the requested task.\nVerification: successful verification command completed.".into()
                }],
                stop_reason: None,
                input_tokens: 25,
                output_tokens: 25,
                cache_usage: None,
                provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None
            });
        }

        let mut calls = self.calls.lock().await;
        *calls += 1;

        let blocks = match *calls {
            1 => vec![
                ModelBlock::ToolUse {
                    id: "patch".into(),
                    name: "ApplyPatch".into(),
                    input: serde_json::json!({
                        "patch": "--- a/app.txt\n+++ b/app.txt\n@@ -1,1 +1,1 @@\n-before\n+after\n"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                },
                ModelBlock::ToolUse {
                    id: "verify".into(),
                    name: "ExecCommand".into(),
                    input: serde_json::json!({
                        "cmd": "printf 'tests passed'",
                        "shell": "sh"
                    }),
                    kind: crate::provider::ModelToolCallKind::Function,
                    provider_data: None,
                },
            ],
            2 => vec![ModelBlock::ToolUse {
                id: "read".into(),
                name: "ExecCommand".into(),
                input: serde_json::json!({
                    "cmd": "cat app.txt",
                    "workdir": "."
                }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }],
            _ => vec![ModelBlock::ToolUse {
                id: "agent".into(),
                name: "GetAgent".into(),
                input: serde_json::json!({}),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }],
        };

        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 25,
            output_tokens: 25,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for SkillReadProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        let blocks = match *calls {
            1 => vec![ModelBlock::ToolUse {
                id: "read-skill".into(),
                name: "ExecCommand".into(),
                input: serde_json::json!({
                    "cmd": "cat .agents/skills/demo/SKILL.md",
                    "workdir": "."
                }),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }],
            _ => vec![ModelBlock::Text {
                text: "Skill loaded and applied.".into(),
            }],
        };

        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 20,
            output_tokens: 20,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for SkillActivationCommandProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;

        let blocks = match *calls {
            1 => vec![ModelBlock::ToolUse {
                id: "activate-skill".into(),
                name: self.tool_name.into(),
                input: self.input.clone(),
                kind: crate::provider::ModelToolCallKind::Function,
                provider_data: None,
            }],
            _ => vec![ModelBlock::Text {
                text: "Skill activation observed.".into(),
            }],
        };

        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks,
            stop_reason: None,
            input_tokens: 20,
            output_tokens: 20,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

#[async_trait]
impl AgentProvider for CountingProvider {
    async fn complete_turn(&self, _request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        let mut calls = self.calls.lock().await;
        *calls += 1;
        Ok(ProviderTurnResponse {
            material: Default::default(),
            blocks: vec![ModelBlock::Text {
                text: self.reply.into(),
            }],
            stop_reason: None,
            input_tokens: 10,
            output_tokens: 5,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }
}

/// A DB-only fixture uses the real normalized query selection and its actual
/// ledger guard without creating a Runtime or changing durable qualification.
pub(crate) fn task_query_lease_for_fixture(
    scope: &crate::runtime::resident_input_evidence::InputEvidenceScope,
    owner_epoch: u64,
    spec: crate::runtime::task_queries::TaskQuerySpec<'_>,
    ledger: &crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger,
    records: usize,
    bytes: usize,
) -> anyhow::Result<crate::runtime::task_queries::TaskQueryLease> {
    crate::runtime::task_queries::TaskQueryRegistry::default().register(
        scope,
        owner_epoch,
        spec,
        ledger,
        records,
        bytes,
    )
}
