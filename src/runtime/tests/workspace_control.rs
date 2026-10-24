use super::super::*;
use super::support::*;

use std::process::Command;

use crate::runtime::workspace_control::{
    ExistingWorktreePolicy, WorkspaceSwitchTarget, WorktreeBranchPolicy,
};
use crate::types::{WorkspaceOccupancyRecord, WorktreeProvenance};

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap_or_else(|error| panic!("failed to run git {}: {error}", args.join(" ")));
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_git_repo() -> TempDir {
    let repo = tempdir().unwrap();
    git(repo.path(), &["init", "-b", "main"]);
    git(repo.path(), &["config", "user.name", "Holon Test"]);
    git(
        repo.path(),
        &["config", "user.email", "holon-test@example.com"],
    );
    std::fs::create_dir_all(repo.path().join("src/nested")).unwrap();
    std::fs::write(repo.path().join("src/lib.rs"), "pub fn base() {}\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-m", "initial"]);
    repo
}

struct WorktreeLifecycleProvider {
    steps: Mutex<std::collections::VecDeque<ModelBlock>>,
}

#[async_trait]
impl AgentProvider for WorktreeLifecycleProvider {
    async fn complete_turn(&self, request: ProviderTurnRequest) -> Result<ProviderTurnResponse> {
        assert!(request
            .tools
            .iter()
            .any(|tool| tool.name == "RemoveWorktree"));
        let block = self
            .steps
            .lock()
            .await
            .pop_front()
            .unwrap_or(ModelBlock::Text {
                text: "diagnostic task completed".into(),
            });
        Ok(ProviderTurnResponse {
            blocks: vec![block],
            stop_reason: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_usage: None,
            provider_message_id: None,
            provider_request_id: None,
            request_diagnostics: None,
        })
    }

    fn configured_model_refs(&self) -> Vec<String> {
        vec!["stub".into()]
    }
}

#[tokio::test]
async fn supervised_child_worktree_cleanup_survives_task_completion_and_reload() {
    let home = tempdir().unwrap();
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )
    .unwrap();
    let config = AppConfig::load_with_home(Some(home.path().to_path_buf())).unwrap();
    let provider = Arc::new(WorktreeLifecycleProvider {
        steps: Mutex::new(Default::default()),
    });
    let host = RuntimeHost::new_with_provider(config, provider.clone()).unwrap();
    let parent = host.default_runtime().await.unwrap();
    let repo = init_git_repo();
    let workspace = parent
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    parent
        .switch_workspace_target(
            WorkspaceSwitchTarget::WorkspaceId(workspace.workspace_id.clone()),
            None,
        )
        .await
        .unwrap();
    for (name, input) in [
        (
            "CreateWorktree",
            serde_json::json!({
                "workspace_id": workspace.workspace_id,
                "branch": "feature/child-cleanup",
                "base_ref": "main"
            }),
        ),
        (
            "SwitchWorkspace",
            serde_json::json!({"workspace_id": "agent_home"}),
        ),
    ] {
        provider.steps.lock().await.push_back(ModelBlock::ToolUse {
            id: name.into(),
            name: name.into(),
            input,
            kind: crate::provider::ModelToolCallKind::Function,
            provider_data: None,
        });
    }
    let model_resolution = parent
        .resolve_agent_model_request("InvokeAgent", None)
        .await
        .unwrap();
    let invoked = parent
        .agent_invocation_service()
        .invoke(crate::types::InvokeAgentRequest {
            target: crate::types::InvokeAgentTarget::NewSubagent {
                template: None,
                workspace_mode: crate::types::ChildAgentWorkspaceMode::Inherit,
                model_resolution: Some(model_resolution),
            },
            message: "create a diagnostic worktree, switch home, and finish".into(),
            authority_class: AuthorityClass::OperatorInstruction,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(20), async {
        loop {
            if parent
                .task_record(&invoked.task_handle.task_id)
                .await
                .unwrap()
                .is_some_and(|task| task.status == TaskStatus::Completed)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("child task must complete");
    let root = host
        .runtime_db()
        .execution_root_entries()
        .active_for_workspace(&workspace.workspace_id)
        .unwrap()
        .into_iter()
        .find(|entry| entry.root_kind == WorkspaceProjectionKind::GitWorktreeRoot)
        .expect("child created a registered worktree");
    assert_eq!(
        root.worktree
            .as_ref()
            .unwrap()
            .registered_by_agent_id
            .as_deref(),
        Some(invoked.agent_id.as_str())
    );
    assert!(!host.workspace_occupancies().unwrap().iter().any(|holder| {
        holder.execution_root_id == root.execution_root_id && holder.released_at.is_none()
    }));
    host.unload_runtime(&invoked.agent_id).await;
    let child = host.get_operator_agent(&invoked.agent_id).await.unwrap();
    let identity = child.agent_identity_view().await.unwrap();
    assert_eq!(identity.profile_preset, AgentProfilePreset::PrivateChild);
    assert!(child
        .filtered_tool_specs()
        .await
        .unwrap()
        .iter()
        .any(|tool| tool.name == "RemoveWorktree"));
    // The reloaded creator and its parent use exactly the same cleanup service.
    git(
        repo.path(),
        &["worktree", "lock", root.filesystem_path.to_str().unwrap()],
    );
    let error = child
        .remove_registered_worktree(
            &root.execution_root_id,
            None,
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("locked"), "{error}");
    git(
        repo.path(),
        &["worktree", "unlock", root.filesystem_path.to_str().unwrap()],
    );
    let removed = parent
        .remove_registered_worktree(
            &root.execution_root_id,
            None,
            WorktreeBranchPolicy::DeleteIfMerged,
            Some("main"),
        )
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(removed.branch_deleted);
    assert!(!root.filesystem_path.exists());
}

#[tokio::test]
async fn attach_and_switch_normalize_git_subdirectory() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let subdir = repo.path().join("src/nested");
    let initial_root = runtime.workspace_root();

    let attached = runtime.attach_workspace_path(subdir.clone()).await.unwrap();

    assert_eq!(attached.workspace.workspace_anchor, repo.path());
    assert_eq!(
        attached.discovered_projection_kind,
        WorkspaceProjectionKind::CanonicalRoot
    );
    assert_eq!(runtime.workspace_root(), initial_root);

    let switched = runtime
        .switch_workspace_target(WorkspaceSwitchTarget::Path(subdir.clone()), None)
        .await
        .unwrap();
    assert_eq!(switched.workspace_anchor, repo.path());
    assert_eq!(switched.execution_root, repo.path());
    assert_eq!(switched.cwd, subdir);
}

#[tokio::test]
async fn switch_linked_worktree_uses_origin_workspace_and_registers_discovery() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let linked = repo
        .path()
        .parent()
        .unwrap()
        .join(format!("holon-linked-{}", uuid::Uuid::new_v4().simple()));
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "linked",
            linked.to_str().unwrap(),
            "main",
        ],
    );
    let attached = runtime
        .attach_workspace_path(linked.join("src"))
        .await
        .unwrap();
    assert_eq!(attached.workspace.workspace_anchor, repo.path());
    assert_eq!(
        attached.discovered_projection_kind,
        WorkspaceProjectionKind::GitWorktreeRoot
    );

    let switched = runtime
        .switch_workspace_target(WorkspaceSwitchTarget::Path(linked.join("src")), None)
        .await
        .unwrap();
    assert_eq!(switched.workspace_id, attached.workspace.workspace_id);
    assert_eq!(switched.workspace_anchor, repo.path());
    assert_eq!(switched.execution_root, linked);
    assert_eq!(switched.cwd, linked.join("src"));

    let state = runtime.workspace_state_result().await.unwrap();
    let artifact = state
        .execution_roots
        .iter()
        .find(|entry| entry.execution_root_id == switched.execution_root_id)
        .unwrap();
    assert_eq!(
        artifact.worktree.as_ref().unwrap().provenance,
        WorktreeProvenance::Discovered
    );
}

#[tokio::test]
async fn create_worktree_reuses_unique_live_branch_without_applying_new_base() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;

    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/reuse",
            "main",
            Some("feature-reuse"),
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    assert!(created.created);
    assert!(created.base_ref_applied);
    let original_tip = created.branch_tip.clone().unwrap();

    std::fs::write(repo.path().join("main-only.txt"), "new main\n").unwrap();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-m", "advance main"]);
    let advanced_main = git(repo.path(), &["rev-parse", "main"]);
    assert_ne!(advanced_main, original_tip);

    let reused = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/reuse",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    assert_eq!(reused.disposition, "reused");
    assert!(reused.reused);
    assert!(!reused.created);
    assert!(!reused.base_ref_applied);
    assert_eq!(reused.resolved_base_commit, advanced_main);
    assert_eq!(reused.branch_tip.as_deref(), Some(original_tip.as_str()));
}

#[tokio::test]
async fn create_worktree_reports_branch_only_and_strict_existing_conflicts_without_switching() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let initial_root = runtime.workspace_root();
    git(repo.path(), &["branch", "branch-only", "main"]);

    let branch_only = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "branch-only",
            "main",
            None,
            true,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    assert_eq!(branch_only.disposition, "conflict");
    assert_eq!(
        branch_only.conflict.as_deref(),
        Some("branch exists without a live linked worktree")
    );
    assert_eq!(runtime.workspace_root(), initial_root);

    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "strict-existing",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let strict = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "strict-existing",
            "main",
            None,
            true,
            ExistingWorktreePolicy::Error,
        )
        .await
        .unwrap();
    assert_eq!(strict.disposition, "already_exists");
    assert!(!strict.activated);
    assert_eq!(strict.worktree_path, created.worktree_path);
    assert_eq!(runtime.workspace_root(), initial_root);
}

#[tokio::test]
async fn active_detach_falls_back_to_agent_home_and_retains_artifact() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/detach",
            "main",
            None,
            true,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();

    let detached = runtime
        .detach_workspace_with_fallback(&workspace.workspace_id)
        .await
        .unwrap();

    assert!(detached.switched_to_agent_home);
    assert!(detached
        .retained_execution_roots
        .iter()
        .any(|entry| Some(&entry.execution_root_id) == created.execution_root_id.as_ref()));
    let state = runtime.agent_state().await.unwrap();
    assert_eq!(
        state.active_workspace_entry.unwrap().workspace_id,
        crate::types::agent_home_workspace_id(&state.id)
    );
    assert!(!state
        .attached_workspaces
        .iter()
        .any(|workspace_id| workspace_id == &workspace.workspace_id));
    assert!(runtime
        .workspace_state_result()
        .await
        .unwrap()
        .execution_roots
        .iter()
        .any(|entry| Some(&entry.execution_root_id) == created.execution_root_id.as_ref()));
}

#[tokio::test]
async fn remove_worktree_retains_dirty_then_removes_clean_without_deleting_branch() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/remove",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    let worktree_path = created.worktree_path.unwrap();
    let dirty_file = worktree_path.join("dirty.txt");
    std::fs::write(&dirty_file, "dirty\n").unwrap();

    let retained = runtime
        .remove_registered_worktree(&execution_root_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert_eq!(retained.disposition, "retained_dirty");
    assert!(!retained.removed);
    assert!(retained
        .changed_files
        .iter()
        .any(|path| path == "dirty.txt"));
    assert!(worktree_path.exists());

    std::fs::remove_file(dirty_file).unwrap();
    let removed = runtime
        .remove_registered_worktree(&execution_root_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!removed.branch_deleted);
    assert!(!worktree_path.exists());
    assert!(!git(repo.path(), &["branch", "--list", "feature/remove"]).is_empty());
    assert!(runtime
        .workspace_state_result()
        .await
        .unwrap()
        .execution_roots
        .iter()
        .find(|entry| entry.execution_root_id == execution_root_id)
        .unwrap()
        .removed_at
        .is_some());
}

#[tokio::test]
async fn remove_worktree_resolves_registered_path_selector() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/remove-by-path",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    let worktree_path = created.worktree_path.unwrap();

    let removed = runtime
        .remove_registered_worktree_selector(
            None,
            Some(&worktree_path),
            None,
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await
        .unwrap();

    assert_eq!(removed.execution_root_id, execution_root_id);
    assert!(removed.removed);
    assert!(!worktree_path.exists());
}

#[tokio::test]
async fn remove_worktree_keeps_branch_when_merge_reachability_check_fails() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/not-merged",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    let worktree_path = created.worktree_path.unwrap();

    git(repo.path(), &["branch", "unrelated", "main"]);
    std::fs::write(worktree_path.join("feature.txt"), "feature\n").unwrap();
    git(&worktree_path, &["add", "."]);
    git(&worktree_path, &["commit", "-m", "feature commit"]);

    let removed = runtime
        .remove_registered_worktree(
            &execution_root_id,
            None,
            WorktreeBranchPolicy::DeleteIfMerged,
            Some("unrelated"),
        )
        .await
        .unwrap();

    assert_eq!(removed.disposition, "removed_branch_retained");
    assert!(removed.removed);
    assert!(!removed.branch_deleted);
    assert_eq!(
        removed.branch_retained_reason.as_deref(),
        Some("not_ancestor_of_merged_into")
    );
    assert!(!worktree_path.exists());
    assert!(!git(repo.path(), &["branch", "--list", "feature/not-merged"]).is_empty());
}

#[tokio::test]
async fn active_dirty_remove_preflight_does_not_switch_workspace() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/active-dirty",
            "main",
            None,
            true,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    let worktree_path = created.worktree_path.unwrap();
    std::fs::write(worktree_path.join("dirty.txt"), "dirty\n").unwrap();

    let retained = runtime
        .remove_registered_worktree(
            &execution_root_id,
            Some("canonical"),
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await
        .unwrap();

    assert_eq!(retained.disposition, "retained_dirty");
    assert!(!retained.switched);
    assert_eq!(
        runtime
            .agent_state()
            .await
            .unwrap()
            .active_workspace_entry
            .unwrap()
            .execution_root_id,
        execution_root_id
    );
}

#[tokio::test]
async fn active_remove_accepts_canonical_path_as_return_to() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/return-to-path",
            "main",
            None,
            true,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    let worktree_path = created.worktree_path.unwrap();

    let removed = runtime
        .remove_registered_worktree_selector(
            Some(&execution_root_id),
            None,
            Some(repo.path().to_str().unwrap()),
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await
        .unwrap();

    assert!(removed.removed);
    assert!(removed.switched);
    assert!(!worktree_path.exists());
    assert_eq!(
        runtime
            .agent_state()
            .await
            .unwrap()
            .active_workspace_entry
            .unwrap()
            .workspace_id,
        workspace.workspace_id
    );
}

#[tokio::test]
async fn remove_worktree_accepts_historical_other_agent_registration() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/historical-registration",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    let repository = runtime.inner.runtime_db.execution_root_entries();
    let mut entry = repository.get(&execution_root_id).unwrap().unwrap();
    let worktree = entry.worktree.as_mut().unwrap();
    worktree.registered_by_agent_id = Some("another-agent".into());
    worktree.authorized_agent_ids = vec!["another-agent".into()];
    repository.upsert(&entry).unwrap();

    // Both populated historical lists and empty legacy lists are provenance only.
    for legacy_empty_list in [false, true] {
        if legacy_empty_list {
            let worktree = entry.worktree.as_mut().unwrap();
            worktree.authorized_agent_ids.clear();
            repository.upsert(&entry).unwrap();
        }
        let path = created.worktree_path.as_ref().unwrap();
        std::fs::write(path.join("dirty.txt"), "retain until final check\n").unwrap();
        let retained = runtime
            .remove_registered_worktree(&execution_root_id, None, WorktreeBranchPolicy::Keep, None)
            .await
            .unwrap();
        assert_eq!(retained.disposition, "retained_dirty");
        assert!(path.exists());
        std::fs::remove_file(path.join("dirty.txt")).unwrap();
    }
    let removed = runtime
        .remove_registered_worktree(&execution_root_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!created.worktree_path.unwrap().exists());
}

#[tokio::test]
async fn remove_worktree_requires_caller_binding_for_ids_paths_and_tombstones() {
    let (_home, host, creator) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = creator
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = creator
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/binding-scope",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let id = created.execution_root_id.unwrap();
    let path = created.worktree_path.unwrap();
    host.create_named_agent("cleaner", None).await.unwrap();
    let cleaner = host.get_public_agent("cleaner").await.unwrap();
    for by_path in [false, true] {
        let error = cleaner
            .remove_registered_worktree_selector(
                (!by_path).then_some(id.as_str()),
                by_path.then_some(path.as_path()),
                None,
                WorktreeBranchPolicy::Keep,
                None,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not attached"), "{error}");
        assert!(path.exists());
    }
    assert!(!cleaner
        .agent_state()
        .await
        .unwrap()
        .attached_workspaces
        .contains(&workspace.workspace_id));
    cleaner
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap();
    let canonical = creator
        .switch_workspace_target(
            WorkspaceSwitchTarget::WorkspaceId(workspace.workspace_id.clone()),
            None,
        )
        .await
        .unwrap();
    let error = cleaner
        .remove_registered_worktree(
            &canonical.execution_root_id,
            None,
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("canonical"), "{error}");
    creator
        .switch_workspace_target(WorkspaceSwitchTarget::ExecutionRootId(id.clone()), None)
        .await
        .unwrap();
    let occupied = creator.agent_state().await.unwrap().active_workspace_entry;
    let error = cleaner
        .remove_registered_worktree(&id, Some("agent_home"), WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("occupancy"), "{error}");
    assert_eq!(
        creator.agent_state().await.unwrap().active_workspace_entry,
        occupied
    );
    assert!(path.exists());
    creator
        .switch_workspace_target(
            WorkspaceSwitchTarget::WorkspaceId(workspace.workspace_id.clone()),
            None,
        )
        .await
        .unwrap();
    let before = creator.agent_state().await.unwrap().active_workspace_entry;
    let removed = cleaner
        .remove_registered_worktree_selector(
            None,
            Some(&path),
            None,
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await
        .unwrap();
    assert!(removed.removed);
    assert!(!path.exists());
    assert_eq!(
        creator.agent_state().await.unwrap().active_workspace_entry,
        before
    );
    cleaner
        .detach_workspace_with_fallback(&workspace.workspace_id)
        .await
        .unwrap();
    let error = cleaner
        .remove_registered_worktree(&id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not attached"), "{error}");
    let repeated = creator
        .remove_registered_worktree(&id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert_eq!(repeated.disposition, "already_removed");
}

#[tokio::test]
async fn remove_worktree_rejects_extra_same_agent_occupancy() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/occupied",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let execution_root_id = created.execution_root_id.unwrap();
    runtime
        .inner
        .runtime_db
        .workspace_occupancies()
        .upsert(&WorkspaceOccupancyRecord {
            occupancy_id: crate::ids::workspace_occupancy_id(),
            execution_root_id: execution_root_id.clone(),
            workspace_id: workspace.workspace_id,
            holder_agent_id: runtime.agent_id().await.unwrap(),
            access_mode: WorkspaceAccessMode::SharedRead,
            acquired_at: chrono::Utc::now(),
            released_at: None,
        })
        .unwrap();

    let error = runtime
        .remove_registered_worktree(&execution_root_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("active occupancy"));
    assert!(created.worktree_path.unwrap().exists());
}

#[tokio::test]
async fn removed_path_reuse_gets_new_generation_and_old_id_cannot_remove_it() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "feature/generation-one",
            "main",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .unwrap();
    let old_id = created.execution_root_id.unwrap();
    let path = created.worktree_path.unwrap();
    let removed = runtime
        .remove_registered_worktree(&old_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert!(removed.removed);

    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "-b",
            "feature/generation-two",
            path.to_str().unwrap(),
            "main",
        ],
    );
    let switched = runtime
        .switch_workspace_target(WorkspaceSwitchTarget::Path(path.clone()), None)
        .await
        .unwrap();
    assert_ne!(switched.execution_root_id, old_id);

    let old_result = runtime
        .remove_registered_worktree(&old_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert_eq!(old_result.disposition, "already_removed");
    assert!(path.exists());
}

#[tokio::test]
async fn workspace_state_refresh_preserves_retained_roots_and_removed_evidence() {
    let (_home, _host, runtime) = host_backed_test_runtime().await;
    let repo = init_git_repo();
    let roots_dir = tempdir().unwrap();
    let workspace = runtime
        .attach_workspace_path(repo.path().to_path_buf())
        .await
        .unwrap()
        .workspace;
    let mut roots = std::collections::HashMap::new();
    for label in [
        "removed",
        "missing",
        "status-failed",
        "clean",
        "dirty",
        "detached",
        "other-agent",
    ] {
        let path = roots_dir.path().join(label);
        if label == "detached" {
            git(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "--detach",
                    path.to_str().unwrap(),
                    "main",
                ],
            );
        } else {
            git(
                repo.path(),
                &[
                    "worktree",
                    "add",
                    "-b",
                    label,
                    path.to_str().unwrap(),
                    "main",
                ],
            );
        }
        let switched = runtime
            .switch_workspace_target(WorkspaceSwitchTarget::Path(path.clone()), None)
            .await
            .unwrap();
        roots.insert(label, (switched.execution_root_id, path));
    }
    let removed = runtime
        .remove_registered_worktree(&roots["removed"].0, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap();
    assert!(removed.removed);
    git(
        repo.path(),
        &["worktree", "remove", roots["missing"].1.to_str().unwrap()],
    );
    git(
        repo.path(),
        &[
            "worktree",
            "lock",
            "--reason",
            "inventory-test",
            roots["clean"].1.to_str().unwrap(),
        ],
    );
    std::fs::write(roots["dirty"].1.join("dirty.txt"), "dirty\n").unwrap();
    // The inventory still lists this root, but its own status command fails.
    std::fs::remove_file(roots["status-failed"].1.join(".git")).unwrap();
    let repository = runtime.inner.runtime_db.execution_root_entries();
    let mut other = repository.get(&roots["other-agent"].0).unwrap().unwrap();
    other.worktree.as_mut().unwrap().registered_by_agent_id = Some("another-agent".into());
    repository.upsert(&other).unwrap();
    // Inventory results must not manufacture unregistered or unrelated roots.
    let unregistered = roots_dir.path().join("unregistered");
    git(
        repo.path(),
        &[
            "worktree",
            "add",
            "--detach",
            unregistered.to_str().unwrap(),
            "main",
        ],
    );
    runtime
        .detach_workspace_with_fallback(&workspace.workspace_id)
        .await
        .unwrap();

    let state = runtime.workspace_state_result().await.unwrap();
    assert!(!state
        .attached_workspace_ids
        .contains(&workspace.workspace_id));
    assert!(state
        .workspaces
        .iter()
        .any(|entry| entry.workspace_id == workspace.workspace_id));
    assert!(!state.execution_roots.iter().any(|entry| {
        entry.execution_root_id == roots["other-agent"].0 || entry.filesystem_path == unregistered
    }));
    let root = |label: &str| {
        state
            .execution_roots
            .iter()
            .find(|entry| entry.execution_root_id == roots[label].0)
            .unwrap()
    };
    let clean = root("clean").worktree.as_ref().unwrap();
    assert_eq!(clean.branch.as_deref(), Some("clean"));
    assert_eq!(
        clean.head_commit.as_deref(),
        Some(git(repo.path(), &["rev-parse", "main"]).as_str())
    );
    assert_eq!(clean.last_cleanup.as_ref().unwrap().status, "clean");
    assert_eq!(
        clean.last_cleanup.as_ref().unwrap().error.as_deref(),
        Some("worktree is locked: inventory-test")
    );
    let dirty = root("dirty").worktree.as_ref().unwrap();
    assert_eq!(dirty.last_cleanup.as_ref().unwrap().status, "dirty");
    assert_eq!(
        dirty.last_cleanup.as_ref().unwrap().changed_files,
        ["dirty.txt"]
    );
    let detached = root("detached").worktree.as_ref().unwrap();
    assert!(detached.detached);
    assert!(detached.branch_ref.is_none());
    assert_eq!(detached.last_cleanup.as_ref().unwrap().status, "clean");
    let missing = root("missing").worktree.as_ref().unwrap();
    assert_eq!(
        missing.last_cleanup.as_ref().unwrap().status,
        "inspection_failed"
    );
    assert_eq!(
        missing.last_cleanup.as_ref().unwrap().error.as_deref(),
        Some("worktree is missing from live git state")
    );
    let status_failed = root("status-failed").worktree.as_ref().unwrap();
    let evidence = status_failed.last_cleanup.as_ref().unwrap();
    assert_eq!(evidence.status, "inspection_failed");
    assert!(evidence
        .error
        .as_deref()
        .unwrap()
        .contains("not a git repository"));
    assert_eq!(status_failed.branch.as_deref(), Some("status-failed"));
    let tombstone = serde_json::to_value(root("removed")).unwrap();
    assert!(root("removed").removed_at.is_some());

    // Only the disposable fixture loses Git metadata; a later read must retry.
    std::fs::rename(repo.path().join(".git"), repo.path().join("hidden-git")).unwrap();
    let failed = runtime.workspace_state_result().await;
    std::fs::rename(repo.path().join("hidden-git"), repo.path().join(".git")).unwrap();
    let failed = failed.unwrap();
    for label in ["missing", "status-failed", "clean", "dirty", "detached"] {
        let entry = failed
            .execution_roots
            .iter()
            .find(|entry| entry.execution_root_id == roots[label].0)
            .unwrap();
        let worktree = entry.worktree.as_ref().unwrap();
        assert_eq!(
            worktree.last_cleanup.as_ref().unwrap().status,
            "inspection_failed"
        );
        let previous = root(label).worktree.as_ref().unwrap();
        assert_eq!(worktree.branch, previous.branch);
        assert_eq!(worktree.branch_ref, previous.branch_ref);
        assert_eq!(worktree.head_commit, previous.head_commit);
        assert_eq!(worktree.detached, previous.detached);
    }
    let removed_after_failure = failed
        .execution_roots
        .iter()
        .find(|entry| entry.execution_root_id == roots["removed"].0)
        .unwrap();
    assert_eq!(
        serde_json::to_value(removed_after_failure).unwrap(),
        tombstone
    );

    std::fs::remove_file(roots["dirty"].1.join("dirty.txt")).unwrap();
    git(&roots["dirty"].1, &["checkout", "--detach"]);
    let refreshed = runtime.workspace_state_result().await.unwrap();
    let dirty = refreshed
        .execution_roots
        .iter()
        .find(|entry| entry.execution_root_id == roots["dirty"].0)
        .unwrap()
        .worktree
        .as_ref()
        .unwrap();
    assert!(dirty.detached);
    assert!(dirty.branch_ref.is_none());
    assert_eq!(dirty.last_cleanup.as_ref().unwrap().status, "clean");
    let removed_after_refresh = refreshed
        .execution_roots
        .iter()
        .find(|entry| entry.execution_root_id == roots["removed"].0)
        .unwrap();
    assert_eq!(
        serde_json::to_value(removed_after_refresh).unwrap(),
        tombstone
    );
}
