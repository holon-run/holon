use super::*;
use crate::execution_types::{WorkspaceAccessMode, WorkspaceProjectionKind};
use crate::model_values::ModelRouteRef;
use crate::scheduler::{ScenarioMode, SchedulerScenarioClass};
use chrono::Utc;
use std::path::PathBuf;

#[test]
fn turn_trigger_summary_matches_dispatch_defaults_without_changing_source() {
    for kind in [
        MessageKind::OperatorPrompt,
        MessageKind::ChannelEvent,
        MessageKind::WebhookEvent,
        MessageKind::CallbackEvent,
        MessageKind::TimerTick,
        MessageKind::SystemTick,
        MessageKind::TaskResult,
        MessageKind::TaskStatus,
        MessageKind::InternalFollowup,
        MessageKind::Control,
        MessageKind::BriefAck,
        MessageKind::BriefResult,
    ] {
        let source = MessageEnvelope::new(
            "agent",
            kind,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "source".into(),
            },
        );
        let mut admitted = source.clone();
        admitted.normalize_admission_fields();
        let summary = TurnTriggerSummary::from_message(&source);
        assert_eq!(summary, TurnTriggerSummary::from_message(&admitted));
        assert_eq!(
            summary.trigger_kind,
            Some(admission_trigger_kind_for_message_kind(&source.kind))
        );
        assert!(source.trigger_kind.is_none());
        assert_eq!(summary.origin, source.origin);
        assert_eq!(summary.authority_class, source.authority_class);
    }
}

#[test]
fn turn_trigger_summary_preserves_explicit_trigger_kind() {
    let mut source = MessageEnvelope::new(
        "agent",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "source".into(),
        },
    );
    let default_summary = TurnTriggerSummary::from_message(&source);
    source.trigger_kind = Some(ContinuationTriggerKind::ExternalEvent);
    let explicit_summary = TurnTriggerSummary::from_message(&source);
    assert_eq!(
        explicit_summary.trigger_kind,
        Some(ContinuationTriggerKind::ExternalEvent)
    );
    assert_ne!(explicit_summary, default_summary);
    source.normalize_admission_fields();
    assert_eq!(explicit_summary, TurnTriggerSummary::from_message(&source));
}

#[test]
#[allow(deprecated)]
fn execution_admission_provenance_reads_legacy_compat_history() {
    let provenance: ExecutionAdmissionProvenance = serde_json::from_value(serde_json::json!({
        "kind": "legacy_compat",
        "scenario_class": "exact_wait_resume",
        "effective_mode": "shadow",
    }))
    .unwrap();

    assert_eq!(
        provenance,
        ExecutionAdmissionProvenance::LegacyCompat {
            scenario_class: Some(SchedulerScenarioClass::ExactWaitResume),
            effective_mode: ScenarioMode::Shadow,
        }
    );
}

#[test]
fn operator_origin_reads_ledger_entries_without_display_name() {
    let origin: MessageOrigin =
        serde_json::from_str(r#"{"kind":"operator","actor_id":"control"}"#).unwrap();
    assert_eq!(
        origin,
        MessageOrigin::Operator {
            actor_id: Some("control".into()),
            actor_display_name: None,
        }
    );
}

#[test]
fn operator_origin_display_name_round_trips() {
    let origin = MessageOrigin::Operator {
        actor_id: Some("oidc-user-one".into()),
        actor_display_name: Some("Alice".into()),
    };
    let value = serde_json::to_value(&origin).unwrap();
    assert_eq!(
        value,
        serde_json::json!({
            "kind": "operator",
            "actor_id": "oidc-user-one",
            "actor_display_name": "Alice",
        })
    );
    let decoded: MessageOrigin = serde_json::from_value(value).unwrap();
    assert_eq!(decoded, origin);
}

#[test]
fn agent_invocation_context_requires_caller_when_any_field_is_present() {
    let error = AgentInvocationContext::from_fields(
        None,
        None,
        Some("turn-1".into()),
        None,
        None,
        None,
        true,
    )
    .unwrap_err();
    assert!(error.contains(HOLON_CALLER_AGENT_ID_ENV));
}

#[test]
fn agent_invocation_context_parses_declared_fields_and_authority_aliases() {
    let context = AgentInvocationContext::from_fields(
        Some("agent-a".into()),
        Some("task-1".into()),
        Some("turn-1".into()),
        Some("work-1".into()),
        Some("activation-1".into()),
        Some("trusted-integration".into()),
        true,
    )
    .unwrap()
    .unwrap();

    assert_eq!(context.caller_agent_id, "agent-a");
    assert_eq!(context.source_task_id.as_deref(), Some("task-1"));
    assert_eq!(context.source_turn_id.as_deref(), Some("turn-1"));
    assert_eq!(context.source_work_item_id.as_deref(), Some("work-1"));
    assert_eq!(
        context.source_activation_id.as_deref(),
        Some("activation-1")
    );
    assert_eq!(
        context.inherited_authority_class,
        Some(AuthorityClass::IntegrationSignal)
    );
}

#[test]
fn context_free_agent_invocation_context_is_operator_mode() {
    assert_eq!(
        AgentInvocationContext::from_fields(None, None, None, None, None, None, false).unwrap(),
        None
    );
}

#[test]
fn legacy_scheduler_contract_names_deserialize_as_current_terms() {
    let continuation: ContinuationResolution = serde_json::from_value(serde_json::json!({
        "trigger_kind": "task_result",
        "class": "local_continuation",
        "model_visible": true,
        "prior_closure_outcome": "continuable",
        "prior_waiting_reason": null,
        "matched_waiting_reason": false,
        "evidence": []
    }))
    .unwrap();
    assert!(continuation.model_reentry);

    let priority: Priority = serde_json::from_value(serde_json::json!("interrupt")).unwrap();
    assert_eq!(priority, Priority::Interject);
    assert_eq!(
        serde_json::to_value(priority).unwrap(),
        serde_json::json!("interject")
    );

    let status: QueueEntryStatus =
        serde_json::from_value(serde_json::json!("interrupted")).unwrap();
    assert_eq!(status, QueueEntryStatus::Interrupted);
    assert_eq!(
        serde_json::to_value(status).unwrap(),
        serde_json::json!("interrupted")
    );
}

#[test]
fn agent_state_changed_event_is_bounded_state_summary() {
    let mut state = AgentState::new("agent-a");
    state.status = AgentStatus::AwakeRunning;
    state.current_run_id = Some("run-1".into());
    state.pending = 3;
    state.last_wake_reason = Some("operator_message".into());
    state.turn_index = 42;
    state.current_turn_id = Some("turn-42".into());
    state.current_turn_work_item_id = Some("turn-work".into());
    state.current_work_item_id = Some("work-current".into());
    state.working_memory.archived_episode_count = 9;
    state.attached_workspaces = vec!["ws-a".into(), "ws-b".into()];
    state.active_workspace_entry = Some(ActiveWorkspaceEntry {
        workspace_id: "ws-b".into(),
        workspace_anchor: PathBuf::from("/tmp/ws-b"),
        execution_root_id: "root-b".into(),
        execution_root: PathBuf::from("/tmp/ws-b"),
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        access_mode: WorkspaceAccessMode::ExclusiveWrite,
        cwd: PathBuf::from("/tmp/ws-b"),
        occupancy_id: Some("occupancy-b".into()),
        projection_metadata: Some(WorkspaceProjectionMetadata::ManagedWorktree(
            ManagedWorktreeProjectionMetadata {
                original_cwd: PathBuf::from("/tmp/ws-b"),
                original_branch: "main".into(),
                worktree_path: PathBuf::from("/tmp/ws-b-worktree"),
                worktree_branch: "feature/metadata-test".into(),
            },
        )),
    });
    state.worktree_session = Some(WorktreeSession {
        original_cwd: PathBuf::from("/tmp/ws-b"),
        original_branch: "main".into(),
        worktree_path: PathBuf::from("/tmp/ws-b-worktree"),
        worktree_branch: "feature/state-event".into(),
    });
    state.last_turn_terminal = Some(TurnTerminalRecord {
        turn_id: "turn-41".into(),
        turn_index: 41,
        kind: TurnTerminalKind::Completed,
        reason: None,
        last_assistant_message: Some("assistant output that must stay out of the event".into()),
        no_brief_reason: None,
        checkpoint: None,
        completed_at: Utc::now(),
        duration_ms: 123,
    });

    let payload = serde_json::to_value(AgentStateChangedEvent::from_state(&state)).unwrap();

    assert_eq!(payload["agent_id"], "agent-a");
    assert_eq!(payload["status"], "awake_running");
    assert_eq!(payload["current_run_id"], "run-1");
    assert_eq!(payload["pending"], 3);
    assert_eq!(payload["turn_index"], 42);
    assert_eq!(payload["active_workspace_id"], "ws-b");
    assert_eq!(payload["active_execution_root_id"], "root-b");
    assert_eq!(payload["active_workspace_occupancy_id"], "occupancy-b");
    assert_eq!(payload["worktree_active"], true);
    assert_eq!(payload["last_turn_terminal"]["kind"], "completed");

    assert!(payload.get("working_memory").is_none());
    assert!(payload.get("context_summary").is_none());
    assert!(payload.get("active_workspace_entry").is_none());
    assert!(payload.get("worktree_session").is_none());
    assert!(payload["last_turn_terminal"]
        .get("last_assistant_message")
        .is_none());
}

#[test]
fn legacy_agents_md_global_names_deserialize_to_user_global() {
    let loaded: LoadedAgentsMd = serde_json::from_value(serde_json::json!({
        "global_source": {
            "scope": "global",
            "kind": "agents_md",
            "path": "/tmp/user/.agents/AGENTS.md"
        }
    }))
    .unwrap();
    let source = loaded
        .user_global_source
        .as_ref()
        .expect("legacy global_source should map to user_global_source");
    assert_eq!(source.scope, AgentsMdScope::UserGlobal);
    assert_eq!(source.path, PathBuf::from("/tmp/user/.agents/AGENTS.md"));

    let encoded = serde_json::to_value(&loaded).unwrap();
    assert!(encoded.get("global_source").is_none());
    assert_eq!(
        encoded["user_global_source"]["scope"],
        serde_json::json!("user_global")
    );

    let view: LoadedAgentsMdView = serde_json::from_value(serde_json::json!({
        "global_source": {
            "scope": "global",
            "kind": "agents_md",
            "path": "/tmp/user/.agents/AGENTS.md"
        }
    }))
    .unwrap();
    assert_eq!(
        view.user_global_source
            .expect("legacy view global_source should map to user_global_source")
            .scope,
        AgentsMdScope::UserGlobal
    );
}

#[test]
fn agents_md_duplicate_status_round_trips_without_content() {
    let loaded = LoadedAgentsMd {
        agent_source: Some(AgentsMdSource {
            scope: AgentsMdScope::Agent,
            kind: AgentsMdKind::AgentsMd,
            path: PathBuf::from("/agent/AGENTS.md"),
            content: "private guidance".into(),
        }),
        agent_status: AgentsMdLoadStatus::Loaded,
        workspace_status: AgentsMdLoadStatus::DuplicateOfAgentScope,
        ..LoadedAgentsMd::default()
    };
    let view = LoadedAgentsMdView::from(&loaded);
    assert_eq!(view.agent_status, AgentsMdLoadStatus::Loaded);
    assert_eq!(
        view.workspace_status,
        AgentsMdLoadStatus::DuplicateOfAgentScope
    );
    assert!(view.workspace_source.is_none());
    for json in [
        serde_json::to_value(&loaded).unwrap(),
        serde_json::to_value(&view).unwrap(),
    ] {
        assert_eq!(json["workspace_status"], "duplicate_of_agent_scope");
        assert!(!json.to_string().contains("private guidance"));
        let decoded: LoadedAgentsMd = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(
            decoded.workspace_status,
            AgentsMdLoadStatus::DuplicateOfAgentScope
        );
        let decoded: LoadedAgentsMdView = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, view);
    }
    let legacy: LoadedAgentsMdView = serde_json::from_str("{}").unwrap();
    assert_eq!(legacy.workspace_status, AgentsMdLoadStatus::NotEvaluated);
}

#[test]
fn new_messages_carry_authority_class() {
    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: Some("operator:jolestar".into()),
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "ship it".into(),
        },
    );

    assert_eq!(message.authority_class, AuthorityClass::OperatorInstruction);
    let serialized = serde_json::to_value(&message).unwrap();
    assert_eq!(serialized["authority_class"], "operator_instruction");
}

#[test]
fn legacy_messages_without_authority_class_deserialize_from_trust_alias() {
    let legacy = serde_json::json!({
        "id": "msg-legacy",
        "agent_id": "default",
        "created_at": "2026-04-22T00:00:00Z",
        "kind": "system_tick",
        "origin": {
            "kind": "system",
            "subsystem": "runtime"
        },
        "trust": "trusted_system",
        "priority": "normal",
        "body": {
            "type": "text",
            "text": "resume"
        }
    });

    let message: MessageEnvelope = serde_json::from_value(legacy).unwrap();

    assert_eq!(message.authority_class, AuthorityClass::RuntimeInstruction);
    assert_eq!(message.trace_context, None);
}

#[test]
fn message_trace_context_round_trips_without_changing_legacy_shape() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::SystemTick,
        MessageOrigin::System {
            subsystem: "runtime".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "resume".into(),
        },
    );
    message.trace_context = Some(
        TraceContext::parse(
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
            Some("vendor=value"),
        )
        .unwrap(),
    );

    let serialized = serde_json::to_value(&message).unwrap();
    let decoded: MessageEnvelope = serde_json::from_value(serialized).unwrap();

    assert_eq!(decoded.trace_context, message.trace_context);
}

#[test]
fn message_with_both_trust_and_authority_class_deserializes_authority_class_first() {
    // When both "trust" and "authority_class" exist, the explicit
    // authority_class field takes precedence.
    let dual = serde_json::json!({
        "id": "msg-dual",
        "agent_id": "default",
        "created_at": "2026-05-27T11:11:59Z",
        "kind": "operator_prompt",
        "origin": {
            "kind": "operator",
            "actor_id": "control"
        },
        "trust": "trusted_operator",
        "authority_class": "operator_instruction",
        "priority": "interject",
        "body": {
            "type": "text",
            "text": "hello"
        }
    });

    let message: MessageEnvelope = serde_json::from_value(dual).unwrap();
    assert_eq!(message.authority_class, AuthorityClass::OperatorInstruction);
}

#[test]
fn legacy_trust_helper_field_is_not_wire_compatible() {
    let helper_field = serde_json::json!({
        "id": "msg-helper-field",
        "agent_id": "default",
        "created_at": "2026-05-27T11:11:59Z",
        "kind": "system_tick",
        "origin": {
            "kind": "system",
            "subsystem": "runtime"
        },
        "legacy_trust": "trusted_system",
        "priority": "normal",
        "body": {
            "type": "text",
            "text": "hello"
        }
    });

    let err = serde_json::from_value::<MessageEnvelope>(helper_field).unwrap_err();
    assert!(
        err.to_string().contains("authority_class"),
        "unexpected error: {err}"
    );
}

#[test]
fn legacy_task_recovery_spec_deserializes_from_trust_alias() {
    let legacy = serde_json::json!({
        "kind": "child_agent_task",
        "summary": "run sub-agent",
        "prompt": "do work",
        "trust": "trusted_operator",
        "workspace_mode": "inherit"
    });

    let spec: TaskRecoverySpec = serde_json::from_value(legacy).unwrap();

    match spec {
        TaskRecoverySpec::ChildAgentTask {
            authority_class,
            lifecycle_disposition,
            ..
        } => {
            assert_eq!(authority_class, AuthorityClass::OperatorInstruction);
            assert_eq!(
                lifecycle_disposition,
                AgentLifecycleDisposition::DeleteOnTerminal
            );
        }
        other => panic!("expected ChildAgentTask, got {:?}", other),
    }
}

#[test]
fn legacy_agent_invocation_recovery_retains_created_subagent() {
    let legacy = serde_json::json!({
        "kind": "agent_invocation",
        "summary": "invoke reusable subagent",
        "prompt": "do work",
        "authority_class": "operator_instruction",
        "target_agent_id": "child-reusable",
        "created_new_subagent": true,
        "workspace_mode": "inherit"
    });

    let spec: TaskRecoverySpec = serde_json::from_value(legacy).unwrap();

    match spec {
        TaskRecoverySpec::AgentInvocation {
            lifecycle_disposition,
            ..
        } => {
            assert_eq!(lifecycle_disposition, AgentLifecycleDisposition::Retain);
        }
        other => panic!("expected AgentInvocation, got {:?}", other),
    }
}

#[test]
fn legacy_tool_execution_record_deserializes_from_trust_alias() {
    let legacy = serde_json::json!({
        "id": "tool-1",
        "agent_id": "default",
        "tool_name": "exec_command",
        "created_at": "2026-04-22T00:00:00Z",
        "duration_ms": 100,
        "trust": "trusted_system",
        "status": "success",
        "input": {},
        "output": {},
        "summary": "ran command"
    });

    let record: ToolExecutionRecord = serde_json::from_value(legacy).unwrap();
    assert_eq!(record.authority_class, AuthorityClass::RuntimeInstruction);
}

#[test]
fn legacy_agent_model_state_deserializes_without_resolved_policy_fields() {
    let legacy = serde_json::json!({
        "source": "runtime_default",
        "runtime_default_model": "anthropic/claude-sonnet-4-6",
        "effective_model": "anthropic/claude-sonnet-4-6",
        "effective_fallback_models": [],
        "override_model": null
    });

    let model: AgentModelState = serde_json::from_value(legacy).unwrap();

    assert_eq!(
        model.effective_model,
        ModelRouteRef::parse("anthropic@default/claude-sonnet-4-6").unwrap()
    );
    assert_eq!(
        model.resolved_policy.source,
        crate::model_values::ModelMetadataSource::UnknownFallback
    );
}

#[test]
fn task_kind_serializes_as_stable_snake_case_string() {
    assert_eq!(
        serde_json::to_value(TaskKind::CommandTask).unwrap(),
        serde_json::json!("command_task")
    );
    assert_eq!(
        serde_json::to_value(TaskKind::ChildAgentTask).unwrap(),
        serde_json::json!("child_agent_task")
    );
    assert_eq!(
        serde_json::to_value(TaskKind::SleepJob).unwrap(),
        serde_json::json!("sleep_job")
    );
}

#[test]
fn task_target_projection_keeps_legacy_identity_unknown_and_rejects_malformed_evidence() {
    let mut task: TaskRecord = serde_json::from_value(serde_json::json!({
        "id": "historical-task",
        "agent_id": "parent",
        "kind": "actor_invocation",
        "status": "completed",
        "created_at": "2026-10-15T00:00:00Z",
        "updated_at": "2026-10-15T00:00:00Z",
        "parent_message_id": null,
        "summary": null,
        "detail": {
            "created_new_subagent": true,
            "child_agent_id": "reused-id"
        }
    }))
    .unwrap();
    assert_eq!(
        task.target(),
        Some(TaskTargetRef {
            kind: TaskTargetKind::ChildAgent,
            agent_id: "reused-id".into(),
            incarnation: None,
        })
    );
    assert_eq!(
        TaskStatusSnapshot::from_task_record(&task).target,
        task.target()
    );
    task.kind = TaskKind::AgentMessageWait;
    task.detail = Some(serde_json::json!({"target_agent_id": "reused-id"}));
    assert_eq!(
        task.target().unwrap().kind,
        TaskTargetKind::AgentMessageWait
    );
    assert_eq!(task.target().unwrap().incarnation, None);
    task.kind = TaskKind::CommandTask;
    assert_eq!(task.target(), None);
    task.kind = TaskKind::ChildAgentTask;
    task.detail = Some(serde_json::json!({
        "child_agent_id": "reused-id",
        "target": {"kind": "invalid", "agent_id": "must-not-fallback"}
    }));
    assert_eq!(task.target(), None);
}

#[test]
fn legacy_task_kind_records_deserialize_without_losing_workspace_mode() {
    let legacy = serde_json::json!({
        "id": "task-legacy",
        "agent_id": "default",
        "kind": "worktree_subagent_task",
        "status": "running",
        "created_at": "2026-04-22T00:00:00Z",
        "updated_at": "2026-04-22T00:00:00Z",
        "parent_message_id": null,
        "summary": "legacy worktree child",
        "detail": null,
        "recovery": null
    });

    let task: TaskRecord = serde_json::from_value(legacy).unwrap();

    assert_eq!(task.kind, TaskKind::WorktreeSubagentTask);
    assert!(task.is_child_agent_task());
    assert_eq!(
        task.child_agent_workspace_mode(),
        Some(ChildAgentWorkspaceMode::Worktree)
    );
    assert_eq!(task.wait_policy(), TaskWaitPolicy::Background);
    assert!(!task.is_blocking());
}

#[test]
fn legacy_child_agent_compat_records_do_not_block_scheduler() {
    let now = Utc::now();
    for (kind, recovery) in [
        (
            TaskKind::SubagentTask,
            TaskRecoverySpec::SubagentTask {
                summary: "legacy inherited".into(),
                prompt: "resume".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            },
        ),
        (
            TaskKind::WorktreeSubagentTask,
            TaskRecoverySpec::WorktreeSubagentTask {
                summary: "legacy worktree".into(),
                prompt: "resume".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            },
        ),
    ] {
        let task = TaskRecord {
            id: format!("task-{kind}"),
            agent_id: "default".into(),
            kind,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: None,
            detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
            recovery: Some(recovery),
        };
        assert_eq!(task.wait_policy(), TaskWaitPolicy::Background);
        assert!(!task.is_blocking());
    }
}

#[test]
fn legacy_command_continue_on_result_recovery_is_terminal_reentry_not_blocking() {
    let recovery = TaskRecoverySpec::CommandTask {
        summary: "long command".into(),
        spec: serde_json::from_value(serde_json::json!({
            "cmd": "sleep 10",
            "workdir": null,
            "shell": null,
            "login": true,
            "tty": false,
            "yield_time_ms": 100,
            "max_output_tokens": null,
            "accepts_input": false,
            "continue_on_result": true
        }))
        .expect("legacy command task spec"),
        authority_class: AuthorityClass::OperatorInstruction,
        promoted_from_exec_command: true,
    };
    let task = TaskRecord {
        id: "task-command".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("long command".into()),
        detail: Some(serde_json::json!({
            "continue_on_result": true,
            "terminal_reentry": true
        })),
        recovery: Some(recovery),
    };

    assert_eq!(task.wait_policy(), TaskWaitPolicy::Background);
    assert!(!task.is_blocking());
    assert!(task.terminal_reentry());

    let snapshot = TaskStatusSnapshot::from_task_record(&task);
    let command = snapshot.command.expect("command task snapshot");
    assert_eq!(command.terminal_reentry, Some(true));

    let recovered_task = TaskRecord {
        detail: None,
        ..task
    };
    assert_eq!(recovered_task.wait_policy(), TaskWaitPolicy::Background);
    assert!(!recovered_task.is_blocking());
    assert!(recovered_task.terminal_reentry());
    assert_eq!(
        TaskStatusSnapshot::from_task_record(&recovered_task)
            .command
            .expect("command task snapshot")
            .terminal_reentry,
        Some(true)
    );
}

#[test]
fn task_status_snapshot_includes_command_identity_fields() {
    let cmd = "git status --short";
    let digest = crate::task::command_digest(cmd);
    let task = TaskRecord {
        id: "task-command".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("check git".into()),
        detail: Some(serde_json::json!({
            "cmd": cmd,
            "cmd_digest": digest.clone(),
            "workdir": "/tmp/repo",
            "shell": "/bin/bash",
            "login": true,
            "tty": false,
            "output_path": "/tmp/command-output.txt",
        })),
        recovery: Some(TaskRecoverySpec::CommandTask {
            summary: "check git".into(),
            spec: CommandTaskSpec {
                cmd: cmd.into(),
                workdir: Some("/tmp/repo".into()),
                shell: Some("/bin/bash".into()),
                login: true,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            authority_class: AuthorityClass::OperatorInstruction,
            promoted_from_exec_command: false,
        }),
    };

    let snapshot = TaskStatusSnapshot::from_task_record(&task);
    let command = snapshot.command.expect("command task snapshot");

    assert_eq!(command.cmd.as_deref(), Some(cmd));
    assert_eq!(command.cmd_digest.as_deref(), Some(digest.as_str()));
    assert_eq!(command.workdir.as_deref(), Some("/tmp/repo"));
    assert_eq!(command.shell.as_deref(), Some("/bin/bash"));
    assert_eq!(command.login, Some(true));
    assert_eq!(command.tty, Some(false));
    assert_eq!(
        command.output_path.as_deref(),
        Some("/tmp/command-output.txt")
    );
}

#[test]
fn command_snapshot_falls_back_to_recovery_identity() {
    let cmd = "cargo test --quiet";
    let task = TaskRecord {
        id: "task-command".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("run tests".into()),
        detail: Some(serde_json::json!({
            "output_summary": "still running"
        })),
        recovery: Some(TaskRecoverySpec::CommandTask {
            summary: "run tests".into(),
            spec: CommandTaskSpec {
                cmd: cmd.into(),
                workdir: Some("/tmp/repo".into()),
                shell: Some("/bin/zsh".into()),
                login: false,
                tty: true,
                yield_time_ms: 1_000,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            authority_class: AuthorityClass::OperatorInstruction,
            promoted_from_exec_command: true,
        }),
    };

    let snapshot = TaskStatusSnapshot::from_task_record(&task);
    let command = snapshot.command.expect("command task snapshot");

    assert_eq!(command.cmd.as_deref(), Some(cmd));
    assert_eq!(command.cmd_digest, Some(crate::task::command_digest(cmd)));
    assert_eq!(command.workdir.as_deref(), Some("/tmp/repo"));
    assert_eq!(command.shell.as_deref(), Some("/bin/zsh"));
    assert_eq!(command.login, Some(false));
    assert_eq!(command.tty, Some(true));
    assert_eq!(command.promoted_from_exec_command, Some(true));
    assert_eq!(command.result_summary.as_deref(), Some("still running"));

    let list_command =
        CommandTaskStatusSnapshot::identity_from_task_record(&task).expect("list projection");
    assert_eq!(list_command.cmd.as_deref(), Some(cmd));
    assert_eq!(
        list_command.cmd_digest,
        Some(crate::task::command_digest(cmd))
    );
    assert_eq!(list_command.promoted_from_exec_command, Some(true));
    assert_eq!(list_command.result_summary, None);
    assert_eq!(list_command.output_path, None);
    assert_eq!(list_command.exit_status, None);
}

#[test]
fn child_agent_recovery_is_supervision_not_scheduler_blocking() {
    let task = TaskRecord {
        id: "task-child".into(),
        agent_id: "default".into(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: None,
        summary: Some("delegate child".into()),
        detail: Some(serde_json::json!({ "wait_policy": "blocking" })),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "delegate child".into(),
            prompt: "finish child work".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::DeleteOnTerminal,
        }),
    };

    assert_eq!(task.wait_policy(), TaskWaitPolicy::Background);
    assert!(!task.is_blocking());
    assert_eq!(
        task.child_agent_workspace_mode(),
        Some(ChildAgentWorkspaceMode::Inherit)
    );
}

#[test]
fn runtime_memory_records_default_missing_workspace_id_to_agent_home() {
    let mut brief = serde_json::to_value(BriefRecord::new(
        "default",
        BriefKind::Result,
        "done",
        None,
        None,
    ))
    .unwrap();
    brief.as_object_mut().unwrap().remove("workspace_id");
    let brief: BriefRecord = serde_json::from_value(brief).unwrap();
    assert_eq!(brief.workspace_id, AGENT_HOME_WORKSPACE_ID);
    assert_eq!(brief.turn_index, None);
    assert_eq!(brief.citations, None);

    let mut work_item =
        serde_json::to_value(WorkItemRecord::new("default", "ship", WorkItemState::Open)).unwrap();
    work_item.as_object_mut().unwrap().remove("workspace_id");
    let work_item: WorkItemRecord = serde_json::from_value(work_item).unwrap();
    assert_eq!(work_item.workspace_id, AGENT_HOME_WORKSPACE_ID);

    let legacy_work_item = serde_json::json!({
        "id": "work_legacy",
        "agent_id": "default",
        "objective": "ship",
        "state": "open",
        "plan_status": "ready",
        "plan_artifact": {
            "path": "/tmp/plan.md",
            "hash": "sha256:abc",
            "bytes": 12,
            "updated_at": "2026-04-20T00:00:00Z",
            "preview": "plan",
            "preview_complete": true
        },
        "created_at": "2026-04-20T00:00:00Z",
        "updated_at": "2026-04-20T00:00:00Z"
    });
    let legacy_work_item: WorkItemRecord = serde_json::from_value(legacy_work_item).unwrap();
    let legacy_artifact = legacy_work_item.plan_artifact.unwrap();
    assert_eq!(legacy_artifact.owner_agent_id, "");
    assert_eq!(legacy_artifact.workspace_id, AGENT_HOME_WORKSPACE_ID);
    assert_eq!(legacy_artifact.relative_path, PathBuf::new());

    let episode = serde_json::json!({
        "id": "episode-1",
        "agent_id": "default",
        "created_at": "2026-04-20T00:00:00Z",
        "finalized_at": "2026-04-20T00:01:00Z",
        "start_turn_index": 1,
        "end_turn_index": 2,
        "start_message_count": 1,
        "end_message_count": 2,
        "boundary_reason": "hard_turn_cap",
        "work_summary": "summary",
        "scope_hints": [],
        "summary": "episode summary",
        "working_set_files": [],
        "commands": [],
        "verification": [],
        "decisions": [],
        "carry_forward": [],
        "waiting_on": []
    });
    let episode: ContextEpisodeRecord = serde_json::from_value(episode).unwrap();
    assert_eq!(episode.workspace_id, AGENT_HOME_WORKSPACE_ID);
    let serialized = serde_json::to_value(episode).unwrap();
    assert_eq!(serialized["start_message_count"], 1);
    assert_eq!(serialized["end_message_count"], 2);
}

#[test]
fn legacy_agent_message_count_is_ignored_by_state_comparison() {
    let state = AgentState::new("default");
    let mut legacy = serde_json::to_value(&state).unwrap();
    legacy["total_message_count"] = serde_json::json!(42);
    let loaded: AgentState = serde_json::from_value(legacy).unwrap();
    assert_eq!(state, loaded);
    assert!(serde_json::to_value(loaded)
        .unwrap()
        .get("total_message_count")
        .is_none());
}

#[test]
fn legacy_persistent_identity_maps_to_self_owned_public_named() {
    let legacy = serde_json::json!({
        "agent_id": "default",
        "kind": "default",
        "visibility": "public",
        "durability": "persistent",
        "status": "active",
        "created_at": "2026-04-22T00:00:00Z",
        "updated_at": "2026-04-22T00:00:00Z"
    });
    let record: AgentIdentityRecord = serde_json::from_value(legacy).unwrap();
    let view = AgentIdentityView::from_record(&record, "default");

    assert_eq!(view.ownership, AgentOwnership::SelfOwned);
    assert_eq!(view.profile_preset, AgentProfilePreset::PublicNamed);
    assert!(record.durability.is_some());
}

#[test]
fn legacy_ephemeral_child_identity_maps_to_parent_supervised_private_child() {
    let legacy = serde_json::json!({
        "agent_id": "tmp_child_demo",
        "kind": "child",
        "visibility": "private",
        "durability": "ephemeral",
        "status": "active",
        "parent_agent_id": "default",
        "delegated_from_task_id": "task-1",
        "created_at": "2026-04-22T00:00:00Z",
        "updated_at": "2026-04-22T00:00:00Z"
    });
    let record: AgentIdentityRecord = serde_json::from_value(legacy).unwrap();
    let view = AgentIdentityView::from_record(&record, "default");

    assert_eq!(view.ownership, AgentOwnership::ParentSupervised);
    assert_eq!(view.profile_preset, AgentProfilePreset::PrivateChild);
}

#[test]
fn identity_view_preserves_lineage_separately_from_supervision() {
    let record = AgentIdentityRecord::new(
        "release-bot",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    )
    .with_lineage_parent_agent_id(Some("default".into()));
    let view = AgentIdentityView::from_record(&record, "default");

    assert_eq!(view.parent_agent_id, None);
    assert_eq!(view.delegated_from_task_id, None);
    assert_eq!(view.lineage_parent_agent_id.as_deref(), Some("default"));
}

#[test]
fn private_child_disables_agent_creation_and_authority_expansion_families() {
    assert!(!AgentProfilePreset::PrivateChild
        .allows_tool_capability_family(ToolCapabilityFamily::AgentCreation));
    assert!(!AgentProfilePreset::PrivateChild
        .allows_tool_capability_family(ToolCapabilityFamily::AuthorityExpanding));
    assert!(AgentProfilePreset::PrivateChild
        .allows_tool_capability_family(ToolCapabilityFamily::LocalEnvironment));
    assert!(AgentProfilePreset::PrivateChild
        .allows_tool_capability_family(ToolCapabilityFamily::ExternalTrigger));
}

#[test]
fn public_named_keeps_operator_notification_policy_owned_by_runtime() {
    for family in [
        ToolCapabilityFamily::CoreAgent,
        ToolCapabilityFamily::LocalEnvironment,
        ToolCapabilityFamily::AgentCreation,
        ToolCapabilityFamily::AuthorityExpanding,
        ToolCapabilityFamily::ExternalTrigger,
    ] {
        assert!(AgentProfilePreset::PublicNamed.allows_tool_capability_family(family));
    }
}

#[test]
fn message_lifecycle_audit_event_omits_body_and_metadata() {
    let mut message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator {
            actor_id: None,
            actor_display_name: None,
        },
        AuthorityClass::OperatorInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "large-message-body".repeat(1024),
        },
    );
    message.metadata = Some(serde_json::json!({
        "large_metadata": "large-message-metadata".repeat(1024)
    }));

    let payload = serde_json::to_value(MessageLifecycleAuditEvent::from_message(&message)).unwrap();

    assert_eq!(payload["message_id"], message.id);
    assert!(payload.get("body").is_none());
    assert!(payload.get("metadata").is_none());
    assert!(!payload.to_string().contains("large-message-body"));
    assert!(!payload.to_string().contains("large-message-metadata"));
}

#[test]
fn brief_created_audit_event_omits_full_text_and_attachments() {
    let mut brief = BriefRecord::new(
        "default",
        BriefKind::Result,
        "large-brief-text".repeat(1024),
        Some("msg-1".into()),
        Some("task-1".into()),
    );
    brief.work_item_id = Some("wi-1".into());
    brief.citations = Some(vec![Citation {
        url: "https://example.com/source".into(),
        title: Some("Source".into()),
    }]);
    brief.attachments = Some(vec![BriefAttachment {
        kind: "json".into(),
        name: "large".into(),
        uri: None,
        value: Some(serde_json::json!({
            "large_attachment": "large-attachment-value".repeat(1024)
        })),
    }]);

    let payload = serde_json::to_value(BriefCreatedAuditEvent::from_brief(&brief)).unwrap();

    assert_eq!(payload["brief_id"], brief.id);
    assert_eq!(payload["work_item_id"], "wi-1");
    assert!(payload.get("citations").is_none());
    assert!(payload.get("text").is_none());
    assert!(payload.get("text_preview").is_none());
    assert!(payload.get("attachments").is_none());
    assert_eq!(
        payload["content_char_count"].as_u64().unwrap(),
        brief.text.chars().count() as u64
    );
    assert!(!payload.to_string().contains("large-attachment-value"));
}

#[test]
fn work_item_lifecycle_audit_event_omits_full_record_payload() {
    let mut record = WorkItemRecord::new(
        "default",
        "large-work-item-objective".repeat(1024),
        WorkItemState::Open,
    );
    record.plan_artifact = Some(WorkItemPlanArtifact {
        owner_agent_id: "default".into(),
        workspace_id: AGENT_HOME_WORKSPACE_ID.into(),
        workspace_alias: None,
        relative_path: PathBuf::from("plans/wi.md"),
        path: PathBuf::from("/tmp/plans/wi.md"),
        hash: "hash".into(),
        bytes: 42,
        updated_at: Utc::now(),
        preview: "large-plan-preview".repeat(1024),
        preview_complete: true,
    });
    record.todo_list.push(TodoItem {
        text: "large-todo".repeat(1024),
        state: TodoItemState::Pending,
    });
    record.result_summary = Some("result-summary".into());

    let payload = serde_json::to_value(WorkItemLifecycleAuditEvent::from_work_item(
        "updated", &record,
    ))
    .unwrap();

    assert_eq!(payload["work_item_id"], record.id);
    assert_eq!(payload["action"], "updated");
    assert!(payload.get("record").is_none());
    assert!(payload.get("plan_artifact").is_none());
    assert!(payload.get("todo_list").is_none());
    assert!(payload["objective_preview"].as_str().unwrap().len() < record.objective.len());
    assert!(!payload.to_string().contains("large-plan-preview"));
    assert!(!payload.to_string().contains("large-todo"));
}

#[test]
fn task_lifecycle_audit_event_omits_detail_and_recovery() {
    let long_output = "large-task-output".repeat(1024);
    let task = TaskRecord {
        id: "task-large".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Completed,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        work_item_id: Some("work-1".into()),
        summary: Some("run command".into()),
        detail: Some(serde_json::json!({
            "task_status": "completed",
            "exit_status": 0,
            "output_path": "/tmp/task-large.log",
            "output_summary": long_output,
            "initial_output": "large-initial-output".repeat(1024)
        })),
        recovery: Some(TaskRecoverySpec::CommandTask {
            summary: "run command".into(),
            spec: CommandTaskSpec {
                cmd: "cargo test".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 10_000,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            authority_class: AuthorityClass::OperatorInstruction,
            promoted_from_exec_command: false,
        }),
    };

    let payload = serde_json::to_value(TaskLifecycleAuditEvent::from_task(&task)).unwrap();

    assert_eq!(payload["task_id"], "task-large");
    assert_eq!(payload["output_path"], "/tmp/task-large.log");
    assert!(payload.get("detail").is_none());
    assert!(payload.get("recovery").is_none());
    assert!(!payload.to_string().contains("large-initial-output"));
    assert!(payload.to_string().contains("large-task-output"));
    assert!(
        payload["output_summary_preview"].as_str().unwrap().len()
            < "large-task-output".repeat(1024).len()
    );
}

#[test]
fn failure_artifact_deserializes_legacy_shape_with_taxonomy_defaults() {
    let artifact: FailureArtifact = serde_json::from_value(serde_json::json!({
        "category": "runtime",
        "kind": "runtime_error",
        "summary": "legacy failure",
        "source_chain": [],
        "metadata": {}
    }))
    .unwrap();

    assert_eq!(artifact.category, FailureArtifactCategory::Runtime);
    assert_eq!(artifact.kind, "runtime_error");
    assert_eq!(artifact.domain, None);
    assert_eq!(artifact.retryable, None);
    assert_eq!(artifact.recovery_hint, None);
    assert!(artifact.context.is_empty());
}

#[test]
fn tool_execution_audit_event_references_canonical_record_without_output() {
    let payload = serde_json::to_value(ToolExecutionAuditEvent {
        tool_call_id: "call-1".into(),
        tool_execution_id: "tool-1".into(),
        agent_id: "default".into(),
        tool_name: "ExecCommand".into(),
        turn_index: 7,
        turn_id: Some("turn-1".into()),
        run_id: Some("run-1".into()),
        work_item_id: Some("work-1".into()),
        status: ToolExecutionStatus::Success,
        duration_ms: 42,
        summary: "command completed".into(),
        input: Some(serde_json::json!({"cmd": "cargo test"})),
        exec_command_cmd: Some("cargo test".into()),
        exec_command_display: Some("cargo test".into()),
        exec_command_batch_items: None,
        exec_command_cost: None,
        exec_command_disposition: Some("completed".into()),
        exit_status: Some(0),
        task_handle: None,
        error: None,
        error_kind: None,
        tool_error: None,
        reason: None,
    })
    .unwrap();

    assert_eq!(payload["tool_execution_id"], "tool-1");
    assert_eq!(payload["input"]["cmd"], "cargo test");
    assert!(payload.get("output").is_none());
    assert!(payload.get("tool_result").is_none());
    assert!(payload.get("exec_command_result").is_none());
    assert!(payload.get("apply_patch_result").is_none());
}
