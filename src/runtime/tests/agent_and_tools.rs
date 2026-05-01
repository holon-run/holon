use super::super::*;
use super::support::*;

#[tokio::test]
async fn agent_summary_reports_agents_md_sources_without_content() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let agent_agents_md = dir.path().join("AGENTS.md");
    let workspace_claude_md = workspace.path().join("CLAUDE.md");
    std::fs::write(&agent_agents_md, "agent-only secret").unwrap();
    std::fs::write(&workspace_claude_md, "workspace-only secret").unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let summary = runtime.agent_summary().await.unwrap();
    assert_eq!(
        summary
            .loaded_agents_md
            .agent_source
            .as_ref()
            .map(|source| source.path.clone()),
        Some(agent_agents_md)
    );
    assert_eq!(
        summary
            .loaded_agents_md
            .workspace_source
            .as_ref()
            .map(|source| source.path.clone()),
        Some(workspace_claude_md)
    );

    let json = serde_json::to_value(&summary).unwrap();
    assert!(json["loaded_agents_md"]["agent_source"]["content"].is_null());
    assert!(json["loaded_agents_md"]["workspace_source"]["content"].is_null());

    let mut legacy_json = json;
    legacy_json
        .as_object_mut()
        .expect("agent summary should serialize as object")
        .remove("lifecycle");
    let decoded: AgentSummary = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(
        decoded.lifecycle,
        crate::types::AgentLifecycleHint::default()
    );
}

#[tokio::test]
async fn loaded_agents_md_uses_active_workspace_entry_anchor_without_legacy_anchor() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let worktree = tempdir().unwrap();
    let workspace_agents_md = workspace.path().join("AGENTS.md");
    std::fs::write(&workspace_agents_md, "workspace rules").unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.active_workspace_entry = Some(ActiveWorkspaceEntry {
            workspace_id: "workspace-1".into(),
            workspace_anchor: workspace.path().to_path_buf(),
            execution_root_id: RuntimeHandle::build_execution_root_id(
                "workspace-1",
                WorkspaceProjectionKind::GitWorktreeRoot,
                worktree.path(),
            )
            .unwrap(),
            execution_root: worktree.path().to_path_buf(),
            projection_kind: WorkspaceProjectionKind::GitWorktreeRoot,
            access_mode: WorkspaceAccessMode::ExclusiveWrite,
            cwd: worktree.path().to_path_buf(),
            occupancy_id: None,
            projection_metadata: None,
        });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let loaded = runtime.loaded_agents_md().await.unwrap();
    assert_eq!(
        loaded
            .workspace_source
            .as_ref()
            .map(|source| source.path.clone()),
        Some(workspace_agents_md)
    );
}

#[tokio::test]
async fn detached_agent_does_not_load_workspace_agents_md() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    std::fs::write(workspace.path().join("AGENTS.md"), "workspace rules").unwrap();

    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        InitialWorkspaceBinding::Detached,
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let loaded = runtime.loaded_agents_md().await.unwrap();
    assert!(loaded.workspace_source.is_none());
}

#[tokio::test]
async fn filtered_tool_specs_keep_exec_command_visible_when_process_execution_disabled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.process_execution_exposed = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let identity = runtime.agent_identity_view().await.unwrap();
    let tools = runtime.filtered_tool_specs(&identity).unwrap();

    assert!(tools.iter().any(|tool| tool.name == "ExecCommand"));
}

#[tokio::test]
async fn filtered_tool_specs_do_not_expose_worktree_discard() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.supports_managed_worktrees = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let identity = runtime.agent_identity_view().await.unwrap();
    let tools = runtime.filtered_tool_specs(&identity).unwrap();

    assert!(!tools.iter().any(|tool| tool.name == "WorktreeTaskDiscard"));
}

#[tokio::test]
async fn filtered_tool_specs_expose_no_public_task_creation_tool() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let identity = runtime.agent_identity_view().await.unwrap();
    let tools = runtime.filtered_tool_specs(&identity).unwrap();

    assert!(!tools.iter().any(|tool| tool.name == "CreateTask"));
}

#[tokio::test]
async fn filtered_tool_specs_keep_spawn_agent_visible_without_host_bridge() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let identity = runtime.agent_identity_view().await.unwrap();

    let tools = runtime.filtered_tool_specs(&identity).unwrap();

    assert!(tools.iter().any(|tool| tool.name == "SpawnAgent"));
}

#[tokio::test]
async fn filtered_tool_specs_hide_agent_creation_family_for_private_child() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let tools = runtime
        .filtered_tool_specs(&private_child_identity("tmp_child_demo"))
        .unwrap();

    assert!(!tools.iter().any(|tool| tool.name == "SpawnAgent"));
}

#[tokio::test]
async fn filtered_tool_specs_keep_use_workspace_visible_for_private_child() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let tools = runtime
        .filtered_tool_specs(&private_child_identity("tmp_child_demo"))
        .unwrap();

    assert!(tools.iter().any(|tool| tool.name == "UseWorkspace"));
    assert!(!tools.iter().any(|tool| tool.name == "EnterWorkspace"));
    assert!(!tools.iter().any(|tool| tool.name == "ExitWorkspace"));
}

#[tokio::test]
async fn filtered_tool_specs_keep_agent_creation_family_for_public_named_agent() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let identity = runtime.agent_identity_view().await.unwrap();
    let tools = runtime.filtered_tool_specs(&identity).unwrap();

    assert!(tools.iter().any(|tool| tool.name == "SpawnAgent"));
    assert!(tools.iter().any(|tool| tool.name == "UseWorkspace"));
    assert!(!tools.iter().any(|tool| tool.name == "EnterWorkspace"));
    assert!(!tools.iter().any(|tool| tool.name == "ExitWorkspace"));
}

#[tokio::test]
async fn schedule_command_task_rejects_when_process_execution_disabled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.process_execution_exposed = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let err = runtime
        .schedule_command_task(
            "demo".into(),
            crate::types::CommandTaskSpec {
                cmd: "printf test".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 100,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("process execution is disabled"));
}

#[tokio::test]
async fn schedule_inherited_child_agent_task_rejects_when_background_tasks_disabled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.allow_background_tasks = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let err = runtime
        .schedule_child_agent_task(
            "demo".into(),
            "prompt".into(),
            TrustLevel::TrustedOperator,
            crate::types::ChildAgentWorkspaceMode::Inherit,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("background tasks are disabled"));
}

#[tokio::test]
async fn schedule_command_task_rejects_when_background_tasks_disabled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.allow_background_tasks = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let err = runtime
        .schedule_command_task(
            "demo".into(),
            crate::types::CommandTaskSpec {
                cmd: "printf test".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 100,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("background tasks are disabled"));
}

#[tokio::test]
async fn stop_command_task_marks_cancelling_before_terminal_cancelled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let task = runtime
        .schedule_command_task(
            "long sleep".into(),
            crate::types::CommandTaskSpec {
                cmd: "sleep 5".into(),
                workdir: None,
                shell: None,
                login: true,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap();

    let stopped = runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await
        .unwrap();
    assert_eq!(stopped.status, TaskStatus::Cancelling);

    let current = runtime.task_record(&task.id).await.unwrap().unwrap();
    assert_eq!(current.status, TaskStatus::Cancelling);

    let not_ready = runtime.task_output(&task.id, false, 0).await.unwrap();
    assert_eq!(
        not_ready.retrieval_status,
        TaskOutputRetrievalStatus::NotReady
    );
    assert_eq!(not_ready.task.status, TaskStatus::Cancelling);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let current = runtime.task_record(&task.id).await.unwrap().unwrap();
        if current.status == TaskStatus::Cancelled {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn second_stop_requests_force_stop_and_runner_terminates_process_before_cancelled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let pid_file = dir.path().join("command-task.pid");
    let command = format!("echo $$ > {}; exec sleep 30", pid_file.display());
    let task = runtime
        .schedule_command_task(
            "force stop command".into(),
            crate::types::CommandTaskSpec {
                cmd: command,
                workdir: None,
                shell: Some("sh".into()),
                login: false,
                tty: false,
                yield_time_ms: 10,
                max_output_tokens: None,
                accepts_input: false,
                continue_on_result: false,
            },
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while !pid_file.exists() {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .to_string();

    let stopped = runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await
        .unwrap();
    assert_eq!(stopped.status, TaskStatus::Cancelling);
    assert_eq!(
        stopped
            .detail
            .as_ref()
            .and_then(|detail| detail.get("cancel_requested"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    let force_stopped = runtime
        .stop_task(&task.id, &TrustLevel::TrustedOperator)
        .await
        .unwrap();
    assert_eq!(force_stopped.status, TaskStatus::Cancelling);
    assert_eq!(
        force_stopped
            .detail
            .as_ref()
            .and_then(|detail| detail.get("force_stop_requested"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );

    let force_stop_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
    let mut saw_cancelling_while_pid_alive = false;
    while tokio::time::Instant::now() < force_stop_deadline {
        let current = runtime.task_record(&task.id).await.unwrap().unwrap();
        let pid_probe = std::process::Command::new("kill")
            .arg("-0")
            .arg(&pid)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        if current.status == TaskStatus::Cancelling && pid_probe.success() {
            saw_cancelling_while_pid_alive = true;
            break;
        }
        if current.status == TaskStatus::Cancelled {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(saw_cancelling_while_pid_alive);

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let current = runtime.task_record(&task.id).await.unwrap().unwrap();
        let pid_probe = std::process::Command::new("kill")
            .arg("-0")
            .arg(&pid)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        if current.status == TaskStatus::Cancelled && !pid_probe.success() {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }

    assert!(!runtime
        .inner
        .task_handles
        .lock()
        .await
        .contains_key(&task.id));
    let pid_probe = std::process::Command::new("kill")
        .arg("-0")
        .arg(&pid)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(!pid_probe.success());

    let current = runtime.task_record(&task.id).await.unwrap().unwrap();
    assert_eq!(current.status, TaskStatus::Cancelled);
    assert_eq!(
        current
            .detail
            .as_ref()
            .and_then(|detail| detail.get("cancelled_reason"))
            .and_then(serde_json::Value::as_str),
        Some("force_stop_requested")
    );
    assert_eq!(
        current
            .detail
            .as_ref()
            .and_then(|detail| detail.get("cancel_requested"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
    assert_eq!(
        current
            .detail
            .as_ref()
            .and_then(|detail| detail.get("force_stop_requested"))
            .and_then(serde_json::Value::as_bool),
        Some(true)
    );
}

#[tokio::test]
async fn cancelling_task_ignores_late_running_status_update() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();

    let task = TaskRecord {
        id: "regression-task".into(),
        agent_id: "default".into(),
        kind: TaskKind::CommandTask,
        status: TaskStatus::Cancelling,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        parent_message_id: None,
        summary: Some("regression task".into()),
        detail: Some(serde_json::json!({
            "task_status": "cancelling",
        })),
        recovery: None,
    };
    runtime.storage().append_task(&task).unwrap();

    let stale_running = TaskRecord {
        status: TaskStatus::Running,
        updated_at: Utc::now(),
        detail: Some(serde_json::json!({
            "task_status": "running",
        })),
        ..task.clone()
    };

    runtime
        .reduce_task_status_message(stale_running)
        .await
        .unwrap();

    let current = runtime.task_record(&task.id).await.unwrap().unwrap();
    assert_eq!(current.status, TaskStatus::Cancelling);
}

#[tokio::test]
async fn latest_task_list_entries_return_compact_projection() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    runtime
        .storage()
        .append_task(&TaskRecord {
            id: "task-list-1".into(),
            agent_id: "default".into(),
            kind: TaskKind::CommandTask,
            status: TaskStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_message_id: None,
            summary: Some("watch logs".into()),
            detail: Some(serde_json::json!({
                "wait_policy": "blocking",
                "cmd": "tail -f app.log",
                "output_path": "/tmp/output.log",
            })),
            recovery: Some(TaskRecoverySpec::CommandTask {
                summary: "watch logs".into(),
                spec: crate::types::CommandTaskSpec {
                    cmd: "tail -f app.log".into(),
                    workdir: None,
                    shell: None,
                    login: true,
                    tty: false,
                    yield_time_ms: 100,
                    max_output_tokens: None,
                    accepts_input: false,
                    continue_on_result: true,
                },
                trust: TrustLevel::TrustedOperator,
                promoted_from_exec_command: false,
            }),
        })
        .unwrap();

    let entries = runtime.latest_task_list_entries().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "task-list-1");
    assert_eq!(entries[0].status, TaskStatus::Running);
    assert_eq!(entries[0].summary.as_deref(), Some("watch logs"));
    assert_eq!(
        entries[0].wait_policy,
        crate::types::TaskWaitPolicy::Blocking
    );
}

#[tokio::test]
async fn enter_git_worktree_root_rejects_when_managed_worktrees_disabled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.supports_managed_worktrees = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }
    let workspace_entry = WorkspaceEntry::new("ws-1", workspace.path().to_path_buf(), None);
    runtime.attach_workspace(&workspace_entry).await.unwrap();

    let err = runtime
        .enter_workspace(
            &workspace_entry,
            crate::system::WorkspaceProjectionKind::GitWorktreeRoot,
            crate::system::WorkspaceAccessMode::ExclusiveWrite,
            None,
            Some("feature-1".into()),
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("git_worktree_root is disabled"));
}

#[tokio::test]
async fn schedule_worktree_child_agent_task_rejects_when_background_tasks_disabled() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.execution_profile.allow_background_tasks = false;
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let err = runtime
        .schedule_child_agent_task(
            "demo".into(),
            "prompt".into(),
            TrustLevel::TrustedOperator,
            crate::types::ChildAgentWorkspaceMode::Worktree,
        )
        .await
        .unwrap_err();
    assert!(err.to_string().contains("background tasks are disabled"));
}

#[test]
fn current_input_summary_extracts_body_from_context_section() {
    let prompt = EffectivePrompt {
        identity: AgentIdentityView {
            agent_id: "default".into(),
            kind: AgentKind::Default,
            visibility: AgentVisibility::Public,
            ownership: AgentOwnership::SelfOwned,
            profile_preset: AgentProfilePreset::PublicNamed,
            status: AgentRegistryStatus::Active,
            is_default_agent: true,
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
        },
        loaded_agents_md: LoadedAgentsMd::default(),
        cache_identity: crate::prompt::PromptCacheIdentity {
            agent_id: "default".into(),
            prompt_cache_key: "default".into(),
            working_memory_revision: 1,
            compression_epoch: 0,
        },
        system_sections: vec![],
        context_sections: vec![PromptSection {
            name: "current_input".into(),
            id: "current_input".into(),
            content:
                "Current input:\n- [operator][operator_instruction][OperatorPrompt] Fix the failing benchmark output."
                    .into(),
            stability: PromptStability::AgentScoped,
        }],
        rendered_system_prompt: String::new(),
        rendered_context_attachment: String::new(),
    };

    assert_eq!(
        current_input_summary(&prompt),
        "Fix the failing benchmark output."
    );
}

#[tokio::test]
async fn interactive_turn_keeps_pending_working_memory_delta_when_prompt_omits_it() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("done")),
        "default".into(),
        ContextConfig {
            recent_messages: 4,
            recent_briefs: 4,
            prompt_budget_estimated_tokens: 140,
            ..context_config()
        },
    )
    .unwrap();

    {
        let mut guard = runtime.inner.agent.lock().await;
        guard.state.working_memory.current_working_memory = crate::types::WorkingMemorySnapshot {
            delivery_target: Some("ship the prompt delta gating fix".into()),
            current_plan: vec!["[InProgress] wire prompt render acknowledgement".into()],
            ..crate::types::WorkingMemorySnapshot::default()
        };
        guard.state.working_memory.working_memory_revision = 5;
        guard.state.working_memory.pending_working_memory_delta =
            Some(crate::types::WorkingMemoryDelta {
                from_revision: 4,
                to_revision: 5,
                created_at_turn: 7,
                reason: crate::types::WorkingMemoryUpdateReason::TerminalTurnCompleted,
                changed_fields: vec!["current_plan".into()],
                summary_lines: vec![
                    "updated the current plan with a long-form explanation of why prompt rendering acknowledgement must happen after budgeted assembly rather than before prompt construction".into(),
                    "recorded the continuity decision that pending deltas stay durable across turns until the model actually sees the delta section in a rendered prompt".into(),
                    "captured low-budget prompt coverage for the interactive runtime path that previously cleared the delta too early".into(),
                ],
            });
        runtime.inner.storage.write_agent(&guard.state).unwrap();
    }

    let preview = runtime
        .preview_prompt(
            "Continue the runtime memory work and report the latest status.".into(),
            TrustLevel::TrustedOperator,
        )
        .await
        .unwrap();
    assert!(!preview
        .context_sections
        .iter()
        .any(|section| section.name == "working_memory_delta"));

    let message = MessageEnvelope::new(
        "default",
        MessageKind::OperatorPrompt,
        MessageOrigin::Operator { actor_id: None },
        TrustLevel::TrustedOperator,
        Priority::Normal,
        MessageBody::Text {
            text: "Continue the runtime memory work and report the latest status.".into(),
        },
    );
    runtime
        .process_interactive_message(
            &message,
            None,
            LoopControlOptions {
                max_tool_rounds: None,
            },
        )
        .await
        .unwrap();

    let state = runtime.agent_state().await.unwrap();
    let pending = state
        .working_memory
        .pending_working_memory_delta
        .as_ref()
        .expect("pending delta should remain until rendered");
    assert_eq!(pending.to_revision, 5);
    assert_eq!(
        state.working_memory.last_prompted_working_memory_revision,
        None
    );
}
