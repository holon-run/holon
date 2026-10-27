use super::super::*;
use super::support::*;

use crate::runtime::input_evidence_budget::{
    serialized_bytes, InputMemoryCategory, OwnedBytesEstimate,
};
use crate::runtime::workspace_control::{ExistingWorktreePolicy, WorkspaceSwitchTarget};
use crate::runtime_db::task_snapshot::TaskDomainLoadRequest;
use crate::types::AgentIdentityRecord;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

struct WorktreeFixture {
    _home: TempDir,
    _host: RuntimeHost,
    repo: TempDir,
    runtime: RuntimeHandle,
    root: crate::types::ExecutionRootEntry,
    task: TaskRecord,
}

async fn worktree_fixture() -> WorktreeFixture {
    let (home, host, runtime) = host_backed_test_runtime().await;
    let repo = tempdir().unwrap();
    let repo_path = repo.path().join("repo");
    std::fs::create_dir(&repo_path).unwrap();
    git(&repo_path, &["init", "-b", "main"]);
    git(&repo_path, &["config", "user.name", "Holon Test"]);
    git(
        &repo_path,
        &["config", "user.email", "holon-test@example.com"],
    );
    git(&repo_path, &["commit", "--allow-empty", "-m", "initial"]);
    let workspace = runtime
        .attach_workspace_path(repo_path)
        .await
        .unwrap()
        .workspace;
    runtime
        .switch_workspace_target(
            WorkspaceSwitchTarget::WorkspaceId(workspace.workspace_id.clone()),
            None,
        )
        .await
        .unwrap();
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "task-deletion-test",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Error,
        )
        .await
        .unwrap();
    let root = runtime
        .inner
        .runtime_db
        .execution_root_entries()
        .get(&created.execution_root_id.unwrap())
        .unwrap()
        .unwrap();
    let now = Utc::now();
    let task = TaskRecord {
        id: "deletion-worktree-task".into(),
        agent_id: runtime.agent_id().await.unwrap(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("worktree cleanup safety".into()),
        detail: Some(serde_json::json!({
            "workspace_mode": "worktree",
            "target": {"kind": "child_agent", "agent_id": "worktree-child", "incarnation": 1},
            "worktree": {
                "worktree_path": root.filesystem_path,
                "worktree_branch": "task-deletion-test",
            },
        })),
        recovery: None,
    };
    runtime.inner.runtime_db.tasks().upsert(&task).unwrap();
    WorktreeFixture {
        _home: home,
        _host: host,
        repo,
        runtime,
        root,
        task,
    }
}

async fn cleanup_with_original_owner(
    runtime: &RuntimeHandle,
    task_id: &str,
    detail: &mut serde_json::Value,
) -> worktree::TaskOwnedWorktreeCleanup {
    let mut owner = runtime.begin_mutation().await.unwrap();
    let scope = owner.input_evidence_scope();
    let ledger = &runtime.inner.input_evidence_budget;
    let limits = ledger.limits();
    let request = TaskDomainLoadRequest {
        recovery_at: None,
        query: Default::default(),
        capture: resident_task::TaskCaptureRequest::current(),
        record_limit: usize::try_from(limits.record_limit).unwrap(),
        byte_limit: usize::try_from(
            limits
                .history_per_agent_bytes
                .checked_add(limits.pending_per_agent_bytes)
                .unwrap(),
        )
        .unwrap(),
    };
    let completion = Arc::new(
        ledger
            .reserve_owned_completion(
                &scope,
                InputMemoryCategory::Candidate,
                OwnedBytesEstimate {
                    payload_bytes: serialized_bytes(detail)
                        .unwrap()
                        .checked_add(1024)
                        .unwrap()
                        .checked_mul(8)
                        .unwrap(),
                    metadata_estimate_bytes: 1024,
                },
            )
            .unwrap(),
    );
    let observed = runtime
        .cleanup_original_control_worktree_in_detail(
            task_id,
            detail,
            "task_stop",
            completion.clone(),
            &request,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(!observed.audits.is_empty());
    owner
        .append_original_audit_events(observed.audits)
        .await
        .unwrap();
    observed.cleanup
}

#[tokio::test]
async fn deletion_retention_survives_persistence_and_stale_cleanup_detail() {
    let fixture = worktree_fixture().await;
    let mut task = fixture.task.clone();
    let detail = task.detail.as_mut().unwrap();
    let mut owner = fixture.runtime.begin_mutation().await.unwrap();
    let cleanup = owner
        .retain_task_owned_worktree_for_target_deletion_in_detail(&task.id, detail)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        cleanup.status,
        worktree::TaskOwnedWorktreeCleanupStatus::Retained
    );
    drop(owner);
    task.status = TaskStatus::Cancelled;
    task.updated_at = Utc::now();
    fixture
        .runtime
        .inner
        .runtime_db
        .tasks()
        .upsert(&task)
        .unwrap();

    let persisted = fixture
        .runtime
        .task_record(&task.id)
        .await
        .unwrap()
        .unwrap();
    let worktree = &persisted.detail.unwrap()["worktree"];
    assert_eq!(worktree["cleanup_status"], "retained");
    assert_eq!(worktree["cleanup_reason"], "agent_deleted");
    assert_eq!(worktree["retained_for_review"], true);
    assert_eq!(worktree["cleanup_owner_task_id"], task.id);
    assert_eq!(worktree["auto_cleaned_up"], false);

    let mut stale_detail = fixture.task.detail.unwrap();
    let cleanup = cleanup_with_original_owner(&fixture.runtime, &task.id, &mut stale_detail).await;
    assert_eq!(
        cleanup.status,
        worktree::TaskOwnedWorktreeCleanupStatus::Retained
    );
    assert_eq!(stale_detail["worktree"]["cleanup_reason"], "agent_deleted");
    assert_eq!(stale_detail["worktree"]["cleanup_owner_task_id"], task.id);
    assert!(fixture.root.filesystem_path.exists());
    git(
        &fixture.repo.path().join("repo"),
        &["show-ref", "--verify", "refs/heads/task-deletion-test"],
    );
    assert!(fixture
        .runtime
        .inner
        .runtime_db
        .execution_root_entries()
        .get(&fixture.root.execution_root_id)
        .unwrap()
        .unwrap()
        .removed_at
        .is_none());
}

#[tokio::test]
async fn persisted_target_deletion_fence_retains_clean_task_worktree() {
    let fixture = worktree_fixture().await;
    let identity = AgentIdentityRecord::new(
        "worktree-child",
        AgentKind::Named,
        AgentVisibility::Public,
        AgentOwnership::SelfOwned,
        AgentProfilePreset::PublicNamed,
        None,
        None,
    );
    fixture
        .runtime
        .inner
        .runtime_db
        .agent_identities()
        .upsert(&identity)
        .unwrap();
    fixture
        .runtime
        .inner
        .runtime_db
        .agent_deletions()
        .begin(&identity.agent_id, identity.revision, "test", false)
        .unwrap();
    let mut detail = fixture.task.detail.clone().unwrap();
    // The cleanup caller need not carry the target; it is durable task evidence.
    detail.as_object_mut().unwrap().remove("target");
    let cleanup =
        cleanup_with_original_owner(&fixture.runtime, &fixture.task.id, &mut detail).await;
    assert_eq!(
        cleanup.status,
        worktree::TaskOwnedWorktreeCleanupStatus::Retained
    );
    assert_eq!(detail["worktree"]["cleanup_reason"], "agent_deleted");
    assert_eq!(detail["worktree"]["cleanup_owner_task_id"], fixture.task.id);
    assert!(fixture.root.filesystem_path.exists());
}
