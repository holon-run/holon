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
async fn remove_worktree_rejects_unauthorized_agent_registration() {
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
            "feature/unauthorized",
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

    let error = runtime
        .remove_registered_worktree(&execution_root_id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("is not authorized to remove"));
    assert!(created.worktree_path.unwrap().exists());
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
