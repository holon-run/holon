use super::*;
use crate::{
    config::StateMemoryLimits,
    runtime::resident_task::TaskCaptureRequest,
    system::{ExecutionProfile, WorkspaceAccessMode, WorkspaceProjectionKind, WorkspaceView},
};
use std::process::Command;
fn git(root: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git").args(args).current_dir(root).output()?;
    anyhow::ensure!(
        out.status.success(),
        "git {:?}: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(())
}
struct Repo {
    _dir: tempfile::TempDir,
    root: PathBuf,
    path: PathBuf,
}
impl Repo {
    fn new() -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let root = dir.path().join("repo");
        std::fs::create_dir(&root)?;
        git(&root, &["init", "--quiet"])?;
        git(&root, &["config", "user.email", "fixture@localhost"])?;
        git(&root, &["config", "user.name", "Fixture"])?;
        std::fs::write(root.join("tracked"), "original\n")?;
        git(&root, &["add", "tracked"])?;
        git(&root, &["commit", "--quiet", "-m", "original"])?;
        let path = dir.path().join("worktree");
        git(
            &root,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "original-stop",
                path.to_str().unwrap(),
            ],
        )?;
        Ok(Self {
            _dir: dir,
            root,
            path,
        })
    }
    fn execution(&self) -> Result<EffectiveExecution> {
        Ok(EffectiveExecution {
            profile: ExecutionProfile::default(),
            workspace: WorkspaceView::new(
                None,
                self.root.clone(),
                self.root.clone(),
                self.root.clone(),
                None,
                Some(WorkspaceAccessMode::ExclusiveWrite),
                WorkspaceProjectionKind::CanonicalRoot,
                None,
            )?,
            scope: ExecutionScopeKind::AgentTurn,
            attached_workspaces: Vec::new(),
        })
    }
}
#[tokio::test]
async fn frozen_cleanup_preserves_clean_dirty_branch_and_missing_policies() -> Result<()> {
    for mode in ["clean", "dirty", "branch", "missing"] {
        let repo = Repo::new()?;
        match mode {
            "dirty" => std::fs::write(repo.path.join("tracked"), "uncommitted\n")?,
            "branch" => git(&repo.path, &["checkout", "--quiet", "-b", "another-branch"])?,
            "missing" => git(
                &repo.root,
                &["worktree", "remove", repo.path.to_str().unwrap()],
            )?,
            _ => {}
        }
        let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
        let request = TaskDomainLoadRequest {
            query: Default::default(),
            capture: TaskCaptureRequest::current(),
            record_limit: 4096,
            byte_limit: 4 * 1024 * 1024,
        };
        let frozen = FrozenWorktreeCleanup::new(
            repo.execution()?,
            repo.root.clone(),
            InputEvidenceScope {
                agent_id: "original-parent".into(),
                identity_incarnation: 1,
            },
            ledger.clone(),
            &request,
        )?;
        let mut metadata =
            serde_json::json!({"worktree_path":repo.path,"worktree_branch":"original-stop"});
        let result =
            cleanup_frozen_worktree(frozen, "original-task", &mut metadata, "task_stop").await?;
        assert_eq!(
            result.status,
            match mode {
                "clean" => TaskOwnedWorktreeCleanupStatus::Cleaned,
                "missing" => TaskOwnedWorktreeCleanupStatus::AlreadyRemoved,
                _ => TaskOwnedWorktreeCleanupStatus::Retained,
            }
        );
        assert_eq!(repo.path.exists(), matches!(mode, "dirty" | "branch"));
        assert!(!result.audits.is_empty());
        assert!(ledger.report()?.observed_bytes > 0);
        if mode == "dirty" {
            assert_eq!(metadata["cleanup_reason"], "changed_files");
        }
        if mode == "branch" {
            assert_eq!(metadata["cleanup_reason"], "branch_mismatch");
        }
        if mode == "clean" {
            assert_eq!(metadata["branch_cleanup_status"], "deleted");
        }
        drop(result);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
#[tokio::test]
async fn frozen_cleanup_keeps_a_registered_worktree_when_git_remove_fails() -> Result<()> {
    let repo = Repo::new()?;
    // A locked clean worktree passes branch/status checks, but ordinary remove
    // must fail. The existing policy must never introduce --force.
    git(
        &repo.root,
        &["worktree", "lock", repo.path.to_str().unwrap()],
    )?;
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let request = TaskDomainLoadRequest {
        query: Default::default(),
        capture: TaskCaptureRequest::current(),
        record_limit: 4096,
        byte_limit: 4 * 1024 * 1024,
    };
    let frozen = FrozenWorktreeCleanup::new(
        repo.execution()?,
        repo.root.clone(),
        InputEvidenceScope {
            agent_id: "original-parent".into(),
            identity_incarnation: 1,
        },
        ledger.clone(),
        &request,
    )?;
    let mut metadata =
        serde_json::json!({"worktree_path":repo.path,"worktree_branch":"original-stop"});
    let result =
        cleanup_frozen_worktree(frozen, "original-task", &mut metadata, "task_stop").await?;
    assert_eq!(result.status, TaskOwnedWorktreeCleanupStatus::Failed);
    assert!(repo.path.exists());
    assert!(result
        .audits
        .iter()
        .any(|event| event.kind == "worktree_auto_cleanup_failed"));
    drop(result);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
