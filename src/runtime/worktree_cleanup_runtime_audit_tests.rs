use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};
use crate::types::{TaskKind, TaskStatus, WorkspaceEntry};

fn git(path: &Path, args: &[&str]) -> Result<String> {
    let result = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        result.status.success(),
        "cleanup fixture Git: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(String::from_utf8(result.stdout)?.trim().into())
}
async fn fixture() -> Result<(
    tempfile::TempDir,
    RuntimeHandle,
    ManagedWorktreeSeed,
    TaskRecord,
)> {
    let home = tempdir()?;
    let repo = home.path().join("actual-cleanup-repository");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Cleanup Fixture"])?;
    git(&repo, &["config", "user.email", "cleanup@localhost"])?;
    std::fs::write(repo.join("tracked"), "original\n")?;
    git(&repo, &["add", "tracked"])?;
    git(&repo, &["commit", "-m", "original"])?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "actual-cleanup-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let workspace = WorkspaceEntry::new("actual-cleanup-workspace", repo, None);
    runtime.attach_workspace(&workspace).await?;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            None,
            None,
        )
        .await?;
    let seed = runtime
        .prepare_managed_worktree("original-cleanup-task")
        .await?;
    let at = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::WorktreeSubagentTask,
        status: TaskStatus::Running,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("actual original cleanup task".into()),
        detail: Some(
            serde_json::json!({"workspace_mode":"worktree","worktree":{"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch}}),
        ),
        recovery: None,
    };
    runtime
        .persist_task_transition(&task, "actual_cleanup_task_ack")
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    owner.state.status = AgentStatus::Stopped;
    owner.persist_state(&runtime.inner.storage).await?;
    drop(owner);
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime, seed, task))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_members WHERE family='event'),(SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
fn events(runtime: &RuntimeHandle) -> Result<Vec<AuditEvent>> {
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    Ok(root
        .current_reads
        .as_ref()
        .context("cleanup ACK omitted Current")?
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) => Some(e.clone()),
            _ => None,
        })
        .collect())
}
#[tokio::test]
async fn actual_runtime_cleanup_keeps_each_original_policy_branch_and_current_native_receipt(
) -> Result<()> {
    for mode in [
        "clean",
        "dirty",
        "branch",
        "missing",
        "invalid",
        "inspection",
        "status",
        "locked",
    ] {
        let (_home, runtime, seed, task) = fixture().await?;
        let mut metadata = task.detail.as_ref().unwrap()["worktree"].clone();
        match mode {
            "dirty" => std::fs::write(seed.worktree_path.join("tracked"), "uncommitted\n")?,
            "branch" => {
                git(
                    &seed.worktree_path,
                    &["checkout", "-b", "different-cleanup-branch"],
                )?;
            }
            "missing" => {
                git(
                    &seed.original_cwd,
                    &["worktree", "remove", seed.worktree_path.to_str().unwrap()],
                )?;
            }
            "invalid" => {
                metadata = serde_json::json!({"worktree_branch":seed.worktree_branch});
            }
            "inspection" => {
                let path = seed
                    .original_cwd
                    .parent()
                    .unwrap()
                    .join("not-a-git-repository");
                std::fs::create_dir(&path)?;
                metadata["worktree_path"] = serde_json::json!(path);
            }
            "status" => {
                let index = git(&seed.worktree_path, &["rev-parse", "--git-path", "index"])?;
                std::fs::write(PathBuf::from(index), "actual corrupt index")?;
            }
            "locked" => {
                git(
                    &seed.original_cwd,
                    &["worktree", "lock", seed.worktree_path.to_str().unwrap()],
                )?;
            }
            _ => {}
        }
        let before = counts(&runtime)?;
        let state = runtime.agent_state().await?;
        let result =
            cleanup_runtime_metadata(&runtime, &task.id, &mut metadata, "agent_lifecycle_stop")
                .await?;
        let (status, kind) = match mode {
            "clean" => (
                TaskOwnedWorktreeCleanupStatus::Cleaned,
                "worktree_auto_cleaned_up",
            ),
            "missing" => (
                TaskOwnedWorktreeCleanupStatus::AlreadyRemoved,
                "task_worktree_cleanup_already_removed",
            ),
            "invalid" => (
                TaskOwnedWorktreeCleanupStatus::Failed,
                "task_worktree_cleanup_failed",
            ),
            "locked" => (
                TaskOwnedWorktreeCleanupStatus::Failed,
                "worktree_auto_cleanup_failed",
            ),
            "dirty" => (
                TaskOwnedWorktreeCleanupStatus::Retained,
                "worktree_retained_for_review",
            ),
            _ => (
                TaskOwnedWorktreeCleanupStatus::Retained,
                "task_worktree_cleanup_retained",
            ),
        };
        assert_eq!(result.status, status, "{mode}");
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1),
            "{mode}"
        );
        assert_eq!(
            runtime.agent_state().await?,
            state,
            "cleanup observation cannot mutate State: {mode}"
        );
        assert_eq!(
            runtime.runtime_db().tasks().latest(&task.id)?.as_ref(),
            Some(&task),
            "cleanup cannot manufacture Task terminal/exit: {mode}"
        );
        assert_eq!(
            seed.worktree_path.exists(),
            !matches!(mode, "clean" | "missing"),
            "{mode}"
        );
        let all = events(&runtime)?;
        let event = all
            .iter()
            .filter(|e| e.kind == kind && e.data["task_id"] == task.id)
            .collect::<Vec<_>>();
        assert_eq!(event.len(), 1, "{mode}");
        if mode == "branch" {
            assert_eq!(metadata["cleanup_reason"], "branch_mismatch");
        }
        if mode == "inspection" {
            assert_eq!(metadata["cleanup_reason"], "branch_inspection_failed");
        }
        if mode == "status" {
            assert_eq!(metadata["cleanup_reason"], "change_detection_failed");
        }
        if mode == "dirty" {
            assert_eq!(metadata["cleanup_reason"], "changed_files");
            assert!(!result.changed_files.is_empty());
        }
        if mode == "clean" {
            assert_eq!(metadata["branch_cleanup_status"], "deleted");
        }
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO cleanup_forbidden_audits;ALTER TABLE agent_states RENAME TO cleanup_forbidden_states;ALTER TABLE tasks RENAME TO cleanup_forbidden_tasks;")?;
        assert!(root
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==event[0].id)));
    }
    Ok(())
}
#[tokio::test]
async fn actual_runtime_cleanup_keeps_branch_delete_failure_before_success_observation(
) -> Result<()> {
    let (_home, runtime, seed, task) = fixture().await?;
    let another = seed
        .original_cwd
        .parent()
        .unwrap()
        .join("actual-other-branch-holder");
    // Only fixture setup uses --force to put this branch in two worktrees. The
    // original removal kernel still uses ordinary remove and branch -D.
    git(
        &seed.original_cwd,
        &[
            "worktree",
            "add",
            "--force",
            another.to_str().unwrap(),
            &seed.worktree_branch,
        ],
    )?;
    let mut metadata = task.detail.as_ref().unwrap()["worktree"].clone();
    let before = counts(&runtime)?;
    let result =
        cleanup_runtime_metadata(&runtime, &task.id, &mut metadata, "agent_lifecycle_stop").await?;
    assert_eq!(result.status, TaskOwnedWorktreeCleanupStatus::Cleaned);
    assert!(!seed.worktree_path.exists() && another.exists());
    assert_eq!(metadata["branch_cleanup_status"], "retained");
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
    );
    let all = events(&runtime)?;
    let first = all
        .iter()
        .find(|e| e.kind == "task_worktree_branch_cleanup_retained" && e.data["task_id"] == task.id)
        .unwrap();
    let second = all
        .iter()
        .find(|e| e.kind == "worktree_auto_cleaned_up" && e.data["task_id"] == task.id)
        .unwrap();
    assert!(first.event_seq < second.event_seq);
    assert_eq!(
        runtime.runtime_db().tasks().latest(&task.id)?.as_ref(),
        Some(&task)
    );
    Ok(())
}
#[tokio::test]
async fn accepted_runtime_cleanup_finishes_while_closing_after_caller_abort() -> Result<()> {
    let (_home, runtime, seed, task) = fixture().await?;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !runtime.inner.mutation_tasks.lock().unwrap().all_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let owner = runtime.begin_mutation().await?;
    let before = counts(&runtime)?;
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        let task = task.clone();
        async move {
            let mut metadata = task.detail.unwrap()["worktree"].clone();
            cleanup_runtime_metadata(&runtime, &task.id, &mut metadata, "agent_lifecycle_stop")
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.inner.mutation_tasks.lock().unwrap().all_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    runtime.close_mutation_admission();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    assert!(!seed.worktree_path.exists());
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    assert_eq!(
        runtime.runtime_db().tasks().latest(&task.id)?.as_ref(),
        Some(&task)
    );
    assert_eq!(
        events(&runtime)?
            .iter()
            .filter(|e| e.kind == "worktree_auto_cleaned_up" && e.data["task_id"] == task.id)
            .count(),
        1
    );
    Ok(())
}
#[tokio::test]
async fn failed_cleanup_observation_keeps_actual_physical_result_and_original_error_boundary(
) -> Result<()> {
    let (_home, runtime, seed, task) = fixture().await?;
    let mut metadata = task.detail.as_ref().unwrap()["worktree"].clone();
    let before = counts(&runtime)?;
    // Persistent trigger is visible on the real writer connection.
    runtime.runtime_db().connection()?.execute_batch("CREATE TRIGGER reject_actual_cleanup_audit BEFORE INSERT ON audit_events WHEN NEW.kind='worktree_auto_cleaned_up' BEGIN SELECT RAISE(ABORT,'actual cleanup observation rejected');END;")?;
    let error = cleanup_runtime_metadata(&runtime, &task.id, &mut metadata, "agent_lifecycle_stop")
        .await
        .err()
        .context("actual cleanup Audit trigger did not reject")?;
    assert!(
        format!("{error:#}").contains("actual cleanup observation rejected"),
        "{error:#}"
    );
    assert!(
        !seed.worktree_path.exists(),
        "Audit rollback is not physical undo"
    );
    assert_eq!(
        metadata["cleanup_status"], "cleaned",
        "original branch mutates metadata before Audit failure"
    );
    assert_eq!(
        counts(&runtime)?,
        before,
        "Audit/Current/native/receipt roll back together"
    );
    assert_eq!(
        runtime.runtime_db().tasks().latest(&task.id)?.as_ref(),
        Some(&task)
    );
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER reject_actual_cleanup_audit;")?;
    let result =
        cleanup_runtime_metadata(&runtime, &task.id, &mut metadata, "agent_lifecycle_stop").await?;
    assert_eq!(
        result.status,
        TaskOwnedWorktreeCleanupStatus::AlreadyRemoved
    );
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    assert!(events(&runtime)?
        .iter()
        .all(|e| !(e.kind == "worktree_auto_cleaned_up" && e.data["task_id"] == task.id)));
    Ok(())
}
#[tokio::test]
async fn original_cleanup_request_native_budget_rejects_before_first_metadata_copy_or_git(
) -> Result<()> {
    let (_home, runtime, seed, task) = fixture().await?;
    let mut metadata = task.detail.as_ref().unwrap()["worktree"].clone();
    metadata["large_original_metadata"] = serde_json::json!("m".repeat(17 * 1024 * 1024));
    let before = counts(&runtime)?;
    let ledger = &runtime.inner.input_evidence_budget;
    let charged = ledger.report()?.observed_bytes;
    let error = cleanup_runtime_metadata(&runtime, &task.id, &mut metadata, "agent_lifecycle_stop")
        .await
        .err()
        .context("oversized original cleanup request was copied")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert!(seed.worktree_path.exists());
    assert_eq!(
        metadata["large_original_metadata"].as_str().unwrap().len(),
        17 * 1024 * 1024
    );
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(ledger.report()?.observed_bytes, charged);
    Ok(())
}
