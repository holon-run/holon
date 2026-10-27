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
            recovery_at: None,
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
        recovery_at: None,
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

fn bootstrap_runtime(repo: &Repo) -> Result<RuntimeHandle> {
    RuntimeHandle::new(
        "bootstrap-worktree-owner",
        repo._dir.path().join("agent-home"),
        repo.root.clone(),
        "http://127.0.0.1:7878".into(),
        Arc::new(crate::provider::StubProvider::new("unused")),
        "default".into(),
        crate::runtime::tests::support::context_config(),
    )
}
async fn bootstrap_worktree_task(
    runtime: &RuntimeHandle,
    repo: &Repo,
) -> Result<crate::types::TaskRecord> {
    use crate::types::{TaskKind, TaskRecord, TaskStatus};
    let id = crate::ids::task_id();
    let at = runtime.now();
    let task = TaskRecord {
        id: id.clone(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::WorktreeSubagentTask,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original bootstrap child".into()),
        recovery: None,
        detail: Some(
            runtime
                .task_creation_detail(
                    &id,
                    serde_json::json!({
                        "workspace_mode":"worktree", "custom":"preserved",
                        "worktree":{"worktree_path":repo.path,"worktree_branch":"original-stop"}
                    }),
                )
                .await?,
        ),
    };
    assert_eq!(
        crate::runtime::tasks::unbound_task_rejoin_generation(&task),
        Some(1)
    );
    runtime
        .persist_task_transition(&task, "bootstrap_worktree_actual_fixture")
        .await?;
    Ok(task)
}
async fn stop_bootstrap_agent(runtime: &RuntimeHandle) -> Result<()> {
    let mut owner = runtime.begin_mutation().await?;
    owner.state.status = crate::types::AgentStatus::Stopped;
    owner.persist_state(&runtime.inner.storage).await?;
    Ok(())
}
fn assert_bootstrap_worktree_retained(
    runtime: &RuntimeHandle,
    repo: &Repo,
    original: &crate::types::TaskRecord,
    stopped: bool,
) -> Result<crate::types::TaskRecord> {
    use crate::types::{MessageKind, QueueEntryStatus, TaskStatus};
    let task = runtime
        .runtime_db()
        .tasks()
        .latest(&original.id)?
        .context("original Task missing")?;
    assert_eq!(task.status, TaskStatus::Interrupted);
    assert_eq!(task.created_at, original.created_at);
    assert_eq!(task.agent_id, original.agent_id);
    assert_eq!(task.recovery, original.recovery);
    assert_eq!(
        crate::runtime::tasks::unbound_task_rejoin_generation(&task),
        Some(1)
    );
    assert!(task.rejoin_fence().is_err());
    let detail = task.detail.as_ref().unwrap();
    assert_eq!(detail["custom"], "preserved");
    assert_eq!(
        detail["interrupted_reason"],
        if stopped {
            "agent_stopped"
        } else {
            "runtime_restarted"
        }
    );
    assert_eq!(
        detail["worktree"]["worktree_path"],
        serde_json::json!(repo.path)
    );
    assert_eq!(detail["worktree"]["worktree_branch"], "original-stop");
    if stopped {
        assert_eq!(detail["worktree"]["cleanup_status"], "retained");
        assert_eq!(
            detail["worktree"]["cleanup_reason"],
            "original_child_exit_unproven"
        );
        assert_eq!(detail["worktree"]["retained_for_review"], true);
    } else {
        assert_eq!(
            detail["worktree"],
            original.detail.as_ref().unwrap()["worktree"]
        );
    }
    assert!(detail["worktree"].get("auto_cleaned_up").is_none());
    let result_id = task
        .parent_message_id
        .as_deref()
        .context("original Result missing")?;
    let result = runtime
        .storage()
        .read_message_by_id(result_id)?
        .context("original Result body missing")?;
    assert_eq!(result.kind, MessageKind::TaskResult);
    assert_eq!(result.metadata.as_ref().unwrap()["task_detail"], *detail);
    assert!(runtime
        .storage()
        .read_turn_by_id(result.turn_id.as_deref().unwrap())?
        .is_none());
    assert_eq!(
        runtime
            .runtime_db()
            .queue_entries()
            .latest(result_id)?
            .unwrap()
            .status,
        QueueEntryStatus::Queued
    );
    assert_eq!(runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM audit_events WHERE kind IN ('worktree_auto_cleaned_up','task_worktree_cleanup_already_removed')", [], |row|row.get::<_,usize>(0))?, 0);
    assert!(repo.path.exists());
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo.path)
        .output()?;
    assert!(status.status.success() && status.stdout.is_empty());
    git(
        &repo.root,
        &["show-ref", "--verify", "refs/heads/original-stop"],
    )?;
    Ok(task)
}
fn bootstrap_business_counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM messages),(SELECT COUNT(*) FROM queue_entries),(SELECT COUNT(*) FROM audit_events)", [], |row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?)
}
#[tokio::test]
async fn original_bootstrap_interruption_keeps_clean_worktree_and_unbound_contract_across_restart(
) -> Result<()> {
    for stopped in [false, true] {
        let repo = Repo::new()?;
        let runtime = bootstrap_runtime(&repo)?;
        let task = bootstrap_worktree_task(&runtime, &repo).await?;
        if stopped {
            stop_bootstrap_agent(&runtime).await?;
        }
        drop(runtime);
        let runtime = bootstrap_runtime(&repo)?;
        assert!(runtime.inner.recovered_tasks.lock().await.is_some());
        runtime.bootstrap_recovery(None).await?;
        let interrupted = assert_bootstrap_worktree_retained(&runtime, &repo, &task, stopped)?;
        let counts = bootstrap_business_counts(&runtime)?;
        runtime.bootstrap_recovery(None).await?;
        assert_eq!(bootstrap_business_counts(&runtime)?, counts);
        drop(runtime);
        let runtime = bootstrap_runtime(&repo)?;
        runtime.bootstrap_recovery(None).await?;
        assert_eq!(
            assert_bootstrap_worktree_retained(&runtime, &repo, &task, stopped)?,
            interrupted
        );
        assert_eq!(bootstrap_business_counts(&runtime)?, counts);
    }
    Ok(())
}
#[tokio::test]
async fn stopped_bootstrap_parent_monitor_abort_does_not_prove_independent_child_exit() -> Result<()>
{
    use crate::runtime::{
        command_task::ManagedTaskHandle, control_cancellation::SharedControlTaskHandle,
    };
    let repo = Repo::new()?;
    let runtime = bootstrap_runtime(&repo)?;
    let task = bootstrap_worktree_task(&runtime, &repo).await?;
    let registration = runtime
        .admit_control_task_handle_registration(&task)
        .await?;
    let lease = runtime.inner.control_cancellations.task(&task);
    lease.attach(tokio::spawn(std::future::pending::<()>()));
    runtime.inner.task_handles.lock().await.insert(
        task.id.clone(),
        ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
    );
    drop(registration);
    let (exit, wait) = tokio::sync::oneshot::channel::<()>();
    let (started, observed) = tokio::sync::oneshot::channel::<()>();
    let path = repo.path.clone();
    let child = tokio::spawn(async move {
        let _ = started.send(());
        let _ = wait.await;
        path.exists()
    });
    observed.await?;
    stop_bootstrap_agent(&runtime).await?;
    runtime
        .interrupt_active_tasks_for_lifecycle_stop(vec![task.clone()])
        .await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), lease.join()).await??;
    assert!(
        !child.is_finished(),
        "parent monitor join is not child physical exit"
    );
    assert_bootstrap_worktree_retained(&runtime, &repo, &task, true)?;
    exit.send(())
        .map_err(|_| anyhow!("independent child exited early"))?;
    assert!(child.await?);
    Ok(())
}
