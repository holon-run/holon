use super::super::*;
use super::support::*;
use crate::types::{
    AuthorityClass, MessageBody, MessageKind, MessageOrigin, Priority, TaskKind, TaskStatus,
    WorkItemState,
};

// ── task_from_message ───────────────────────────────────────────────

fn task_message_with_metadata(kind: MessageKind, metadata: serde_json::Value) -> MessageEnvelope {
    let mut msg = MessageEnvelope::new(
        "default",
        kind,
        MessageOrigin::Task {
            task_id: "t1".into(),
        },
        AuthorityClass::RuntimeInstruction,
        Priority::Normal,
        MessageBody::Text {
            text: "task output".into(),
        },
    );
    msg.metadata = Some(metadata);
    msg
}

#[test]
fn task_from_message_missing_task_id_returns_error() {
    let msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({ "task_kind": "command_task" }),
    );
    let result = tasks::task_from_message(&msg, "default");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("task_id"),
        "error should mention missing task_id"
    );
}

#[test]
fn task_from_message_missing_task_kind_returns_error() {
    let msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({ "task_id": "task-1" }),
    );
    let result = tasks::task_from_message(&msg, "default");
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("task_kind"),
        "error should mention missing task_kind"
    );
}

#[test]
fn task_from_message_parses_command_task_with_running_status() {
    let msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({
            "task_id": "cmd-1",
            "task_kind": "command_task",
            "task_status": "running",
            "task_summary": "run tests"
        }),
    );
    let record = tasks::task_from_message(&msg, "agent-1").unwrap();
    assert_eq!(record.id, "cmd-1");
    assert_eq!(record.agent_id, "agent-1");
    assert_eq!(record.kind, TaskKind::CommandTask);
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(record.summary.as_deref(), Some("run tests"));
    assert_eq!(record.parent_message_id.as_deref(), Some(msg.id.as_str()));
}

#[test]
fn task_from_message_infers_running_for_task_status_without_explicit_status() {
    let msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({
            "task_id": "t-2",
            "task_kind": "command_task"
        }),
    );
    let record = tasks::task_from_message(&msg, "default").unwrap();
    assert_eq!(record.status, TaskStatus::Running);
}

#[test]
fn task_from_message_infers_completed_for_task_result_without_explicit_status() {
    let msg = task_message_with_metadata(
        MessageKind::TaskResult,
        serde_json::json!({
            "task_id": "t-3",
            "task_kind": "command_task"
        }),
    );
    let record = tasks::task_from_message(&msg, "default").unwrap();
    assert_eq!(record.status, TaskStatus::Completed);
}

#[test]
fn task_from_message_parses_all_terminal_statuses() {
    for (status_str, expected) in [
        ("cancelling", TaskStatus::Cancelling),
        ("completed", TaskStatus::Completed),
        ("failed", TaskStatus::Failed),
        ("cancelled", TaskStatus::Cancelled),
        ("interrupted", TaskStatus::Interrupted),
    ] {
        let msg = task_message_with_metadata(
            MessageKind::TaskStatus,
            serde_json::json!({
                "task_id": format!("t-{}", status_str),
                "task_kind": "command_task",
                "task_status": status_str
            }),
        );
        let record = tasks::task_from_message(&msg, "default").unwrap();
        assert_eq!(record.status, expected, "status mismatch for {status_str}");
    }
}

#[test]
fn task_from_message_unknown_status_defaults_to_queued() {
    let msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({
            "task_id": "t-unknown",
            "task_kind": "command_task",
            "task_status": "bogus_status"
        }),
    );
    let record = tasks::task_from_message(&msg, "default").unwrap();
    assert_eq!(record.status, TaskStatus::Queued);
}

#[test]
fn task_from_message_extracts_work_item_id_from_metadata() {
    let msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({
            "task_id": "t-wi",
            "task_kind": "command_task",
            "work_item_id": "wi-from-meta"
        }),
    );
    let record = tasks::task_from_message(&msg, "default").unwrap();
    assert_eq!(record.work_item_id.as_deref(), Some("wi-from-meta"));
}

#[test]
fn task_from_message_falls_back_to_message_work_item_id() {
    let mut msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({
            "task_id": "t-wi2",
            "task_kind": "command_task"
        }),
    );
    msg.work_item_id = Some("wi-from-msg".into());
    let record = tasks::task_from_message(&msg, "default").unwrap();
    assert_eq!(record.work_item_id.as_deref(), Some("wi-from-msg"));
}

#[test]
fn task_from_message_preserves_task_detail_and_recovery() {
    let mut msg = task_message_with_metadata(
        MessageKind::TaskStatus,
        serde_json::json!({
            "task_id": "t-detail",
            "task_kind": "command_task",
            "task_detail": { "cancel_requested": true },
            "task_recovery": { "kind": "command_task", "summary": "test cmd", "spec": { "cmd": "true", "workdir": null, "shell": null, "login": false, "tty": false, "yield_time_ms": 0, "max_output_tokens": null, "accepts_input": false, "terminal_reentry": false }, "authority_class": "operator_instruction", "promoted_from_exec_command": false }
        }),
    );
    msg.turn_id = Some("turn-parent".into());
    let record = tasks::task_from_message(&msg, "default").unwrap();
    assert!(record.detail.is_some());
    assert_eq!(
        record.detail.as_ref().unwrap()["cancel_requested"],
        serde_json::json!(true)
    );
    assert_eq!(
        record.detail.as_ref().unwrap()["parent_turn_id"],
        serde_json::json!("turn-parent")
    );
    assert!(record.recovery.is_some());
}

// ── stop_task ───────────────────────────────────────────────────────

#[tokio::test]
async fn stop_task_on_running_command_task_returns_cancelling() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();
    let runtime = RuntimeHandle::new(
        "default",
        dir.path().to_path_buf(),
        workspace.path().to_path_buf(),
        "http://127.0.0.1:7878".into(),
        Arc::new(StubProvider::new("stop-test")),
        "default".into(),
        context_config(),
    )
    .unwrap();
    let task = runtime
        .schedule_command_task(
            "long running".into(),
            crate::types::CommandTaskSpec {
                cmd: "sleep 60".into(),
                workdir: None,
                shell: None,
                login: false,
                tty: false,
                yield_time_ms: 0,
                max_output_tokens: None,
                accepts_input: false,
                terminal_reentry: false,
            },
            AuthorityClass::OperatorInstruction,
        )
        .await
        .unwrap();
    // Wait briefly for the task to start
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let stopped = runtime
        .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
        .await
        .unwrap();
    assert_eq!(stopped.status, TaskStatus::Cancelling);
    assert_eq!(stopped.kind, TaskKind::CommandTask);
    // Clean up: wait for the task to reach terminal state
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let latest = runtime.task_record(&task.id).await.unwrap().unwrap();
        if matches!(
            latest.status,
            TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
        ) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task did not reach terminal state after stop"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn stop_task_on_nonexistent_task_returns_error() {
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
    let result = runtime
        .stop_task("nonexistent-task", &AuthorityClass::OperatorInstruction)
        .await;
    assert!(result.is_err());
}

// ── task_input rejection ────────────────────────────────────────────

#[tokio::test]
async fn task_input_on_nonexistent_task_returns_error() {
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
    let result = runtime.task_input("no-such-task", "hello").await;
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("not found"));
}

// ── complete_work_item idempotency ──────────────────────────────────

#[tokio::test]
async fn complete_work_item_is_idempotent_for_already_completed() {
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
    let item = runtime
        .create_work_item("idempotent test".into(), None, None, Vec::new())
        .await
        .unwrap();
    // Complete it once
    let first = runtime
        .complete_work_item(item.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(first.state, WorkItemState::Completed);
    // Complete it again — should succeed idempotently
    let second = runtime
        .complete_work_item(item.id.clone(), Vec::new())
        .await
        .unwrap();
    assert_eq!(second.state, WorkItemState::Completed);
    assert_eq!(first.id, second.id);
}

// ── pick_work_item error cases ──────────────────────────────────────

#[tokio::test]
async fn pick_completed_work_item_returns_error() {
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
    let item = runtime
        .create_work_item("will be completed".into(), None, None, Vec::new())
        .await
        .unwrap();
    runtime
        .complete_work_item(item.id.clone(), Vec::new())
        .await
        .unwrap();
    let result = runtime.pick_work_item(item.id.clone()).await;
    assert!(result.is_err());
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cannot pick completed"),
        "error should mention cannot pick completed work item"
    );
}

#[tokio::test]
async fn pick_work_item_clear_blocker_requires_non_empty_reason() {
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
    let item = runtime
        .create_work_item("blocker test".into(), None, None, Vec::new())
        .await
        .unwrap();
    // clear_blocker=true with empty reason should fail
    let result = runtime
        .pick_work_item_with_reason_and_clear_blocker(
            item.id.clone(),
            Some("  ".into()), // whitespace-only
            true,
        )
        .await;
    assert!(result.is_err());
    assert!(
        result.unwrap_err().to_string().contains("non-empty reason"),
        "error should require non-empty reason for clear_blocker"
    );
}

// ── pick_work_item yield_current continuation ───────────────────────

#[tokio::test]
async fn pick_second_runnable_work_item_yields_current() {
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
    let first = runtime
        .create_work_item("first runnable".into(), None, None, Vec::new())
        .await
        .unwrap();
    let second = runtime
        .create_work_item("second runnable".into(), None, None, Vec::new())
        .await
        .unwrap();
    // Pick first (becomes current, runnable)
    runtime.pick_work_item(first.id.clone()).await.unwrap();
    // Switch to second — should yield the first via continuation
    let picked = runtime
        .pick_work_item_with_reason(second.id.clone(), None)
        .await
        .unwrap();
    assert_eq!(picked.transition.switch_kind, "yield_current");
    assert!(
        picked.continuation_created.is_some(),
        "continuation frame should be created when yielding current runnable work item"
    );
    let continuation = picked.continuation_created.unwrap();
    assert_eq!(continuation.suspended_work_item_id, first.id);
    assert_eq!(continuation.active_work_item_id, second.id);
}

// ── bootstrap: fresh-start workspace initialization ─────────────────

#[tokio::test]
async fn fresh_start_seeds_active_workspace_entry_from_initial_anchor() {
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
    let state = runtime.inner.agent.lock().await.state.clone();
    let entry = state
        .active_workspace_entry
        .as_ref()
        .expect("active_workspace_entry should be seeded on fresh start");
    assert_eq!(
        entry.workspace_anchor,
        workspace.path().to_path_buf(),
        "workspace anchor should match the initial workspace path"
    );
    assert_eq!(
        entry.projection_kind,
        crate::system::WorkspaceProjectionKind::CanonicalRoot
    );
    assert_eq!(
        entry.access_mode,
        crate::system::WorkspaceAccessMode::ExclusiveWrite
    );
    assert!(
        !state.attached_workspaces.is_empty(),
        "attached_workspaces should be non-empty"
    );
}

#[tokio::test]
async fn fresh_start_initializes_agent_state_with_default_status() {
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
    let state = runtime.inner.agent.lock().await.state.clone();
    assert_eq!(
        state.status,
        crate::types::AgentStatus::AwakeIdle,
        "fresh agent should start in Idle status"
    );
    assert!(state.current_work_item_id.is_none());
    assert!(state.pending_wake_hint.is_none());
    assert!(state.worktree_session.is_none());
}

// ── bootstrap: agent state persistence across restart ───────────────

#[tokio::test]
async fn restart_preserves_persisted_agent_state() {
    let dir = tempdir().unwrap();
    let workspace = tempdir().unwrap();

    // First start: create a work item and pick it
    {
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
        let item = runtime
            .create_work_item("persistent work".into(), None, None, Vec::new())
            .await
            .unwrap();
        runtime.pick_work_item(item.id.clone()).await.unwrap();
    }

    // Second start: should recover the picked work item
    {
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
        let state = runtime.inner.agent.lock().await.state.clone();
        assert!(
            state.current_work_item_id.is_some(),
            "current_work_item_id should be recovered from storage across restart"
        );
    }
}
