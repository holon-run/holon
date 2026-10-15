use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
    workspace_control::{ExistingWorktreePolicy, WorktreeBranchPolicy},
};
fn git(path: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "temporary fixture git: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, WorkspaceEntry)> {
    let home = tempdir()?;
    let repo = home.path().join("control-workspace-repo");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Holon Test"])?;
    git(&repo, &["config", "user.email", "holon-test@example.com"])?;
    git(&repo, &["commit", "--allow-empty", "-m", "initial"])?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "workspace-control-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let attached = runtime.attach_workspace_path(repo).await?;
    assert!(!attached.already_attached);
    assert!(attached.active_unchanged);
    let workspace = attached.workspace;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            None,
            None,
        )
        .await?;
    Ok((home, runtime, workspace))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),
        (SELECT COUNT(*) FROM current_read_members WHERE family='event'),
        (SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
fn events(runtime: &RuntimeHandle, kind: &str) -> Result<Vec<AuditEvent>> {
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let capture = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    Ok(capture
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind == kind => Some(e.clone()),
            _ => None,
        })
        .collect())
}
#[tokio::test]
async fn actual_registered_worktree_create_reuse_and_remove_keep_original_authority_and_audit_order(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let before = counts(&runtime)?;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "actual-created",
            "HEAD",
            None,
            true,
            ExistingWorktreePolicy::Reuse,
        )
        .await?;
    assert!(created.created && !created.reused && created.activated);
    assert!(created.activation_error.is_none());
    let id = created.execution_root_id.as_ref().unwrap();
    let path = created.worktree_path.as_ref().unwrap();
    assert!(path.join(".git").is_file());
    assert_eq!(
        runtime
            .agent_state()
            .await?
            .active_workspace_entry
            .as_ref()
            .unwrap()
            .execution_root_id,
        *id
    );
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 3)
    );
    let written = events(&runtime, "worktree_created")?;
    assert_eq!(written.len(), 1);
    assert_eq!(
        written[0].data,
        serde_json::json!({"workspace_id":workspace.workspace_id,"execution_root_id":id,"branch":"actual-created",
        "requested_base_ref":"HEAD","resolved_base_commit":created.resolved_base_commit,"activated":true})
    );
    let baseline = counts(&runtime)?;
    let reused = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "actual-created",
            "HEAD",
            None,
            true,
            ExistingWorktreePolicy::Reuse,
        )
        .await?;
    assert!(!reused.created && reused.reused && reused.activated);
    assert!(!reused.base_ref_applied);
    assert_eq!(
        counts(&runtime)?,
        baseline,
        "reuse never fabricated the original created audit"
    );
    let removed = runtime
        .remove_registered_worktree(
            id,
            Some(AGENT_HOME_WORKSPACE_ID),
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await?;
    assert!(removed.removed && removed.switched && !removed.branch_deleted);
    assert_eq!(removed.disposition, "removed");
    assert!(!path.exists());
    assert!(workspace.workspace_anchor.join(".git").exists());
    assert!(runtime
        .runtime_db()
        .execution_root_entries()
        .get(id)?
        .unwrap()
        .removed_at
        .is_some());
    git(
        &workspace.workspace_anchor,
        &["show-ref", "--verify", "refs/heads/actual-created"],
    )?;
    assert_eq!(
        counts(&runtime)?,
        (
            baseline.0 + 3,
            baseline.1 + 3,
            baseline.2 + 3,
            baseline.3 + 3
        )
    );
    let removed_event = events(&runtime, "worktree_cleanup_removed")?;
    assert_eq!(removed_event.len(), 1);
    assert_eq!(
        removed_event[0].data,
        serde_json::json!({"execution_root_id":id,"branch":"actual-created","branch_deleted":false,"branch_retained_reason":null})
    );
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO forbidden_worktree_events;ALTER TABLE agent_states RENAME TO forbidden_worktree_states;")?;
    assert!(root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
        .events
        .iter()
        .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==removed_event[0].id)));
    Ok(())
}
#[tokio::test]
async fn actual_dirty_and_locked_registered_worktrees_retain_original_physical_artifacts(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let created = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "actual-dirty",
            "HEAD",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await?;
    let id = created.execution_root_id.as_ref().unwrap();
    let path = created.worktree_path.as_ref().unwrap();
    std::fs::write(path.join("actual-dirty.txt"), "retain original artifact")?;
    let before = counts(&runtime)?;
    let retained = runtime
        .remove_registered_worktree_selector(
            None,
            Some(path),
            None,
            WorktreeBranchPolicy::Keep,
            None,
        )
        .await?;
    assert!(!retained.removed && !retained.switched && !retained.branch_deleted);
    assert_eq!(retained.disposition, "retained_dirty");
    assert!(path.join("actual-dirty.txt").exists());
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    let event = events(&runtime, "worktree_cleanup_retained")?;
    assert_eq!(event.len(), 1);
    assert_eq!(
        event[0].data,
        serde_json::json!({"execution_root_id":id,"changed_files":retained.changed_files})
    );
    assert!(runtime
        .runtime_db()
        .execution_root_entries()
        .get(id)?
        .unwrap()
        .removed_at
        .is_none());
    git(
        &workspace.workspace_anchor,
        &["worktree", "lock", path.to_str().unwrap()],
    )?;
    let before = counts(&runtime)?;
    let error = runtime
        .remove_registered_worktree(id, None, WorktreeBranchPolicy::Keep, None)
        .await
        .err()
        .context("locked artifact removed")?;
    assert!(format!("{error:#}").contains("locked"));
    assert_eq!(counts(&runtime)?, before);
    assert!(path.exists());
    Ok(())
}
#[tokio::test]
async fn accepted_worktree_create_and_return_switch_complete_after_caller_abort_and_closing(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let owner = runtime.begin_mutation().await?;
    let before = counts(&runtime)?;
    let n = runtime
        .inner
        .mutation_tasks
        .lock()
        .unwrap()
        .handles
        .iter()
        .filter(|task| !task.is_finished())
        .count();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        let id = workspace.workspace_id.clone();
        async move {
            runtime
                .create_worktree_for_workspace(
                    &id,
                    "owned-created",
                    "HEAD",
                    None,
                    true,
                    ExistingWorktreePolicy::Reuse,
                )
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime
            .inner
            .mutation_tasks
            .lock()
            .unwrap()
            .handles
            .iter()
            .filter(|task| !task.is_finished())
            .count()
            <= n
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    runtime.close_mutation_admission();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            "must-not-create",
            "HEAD",
            None,
            false,
            ExistingWorktreePolicy::Reuse
        )
        .await
        .is_err());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    let state = runtime.agent_state().await?;
    let entry = state.active_workspace_entry.as_ref().unwrap();
    assert_eq!(
        entry.projection_kind,
        WorkspaceProjectionKind::GitWorktreeRoot
    );
    assert!(entry.execution_root.join(".git").is_file());
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 3)
    );
    assert_eq!(events(&runtime, "worktree_created")?.len(), 1);
    Ok(())
}
#[tokio::test]
async fn actual_path_attach_and_fallback_detach_are_owned_and_preserve_home_protection(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let again = runtime
        .attach_workspace_path(workspace.workspace_anchor.clone())
        .await?;
    assert!(again.already_attached && again.active_unchanged);
    let before = counts(&runtime)?;
    let detached = runtime
        .detach_workspace_with_fallback(&workspace.workspace_id)
        .await?;
    assert!(detached.detached && detached.switched_to_agent_home);
    assert!(detached.retained_execution_roots.is_empty());
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 4, before.1 + 4, before.2 + 4, before.3 + 3)
    );
    let before = counts(&runtime)?;
    assert!(runtime
        .detach_workspace_with_fallback(AGENT_HOME_WORKSPACE_ID)
        .await
        .is_err());
    assert_eq!(counts(&runtime)?, before);
    assert!(workspace.workspace_anchor.join(".git").is_dir());
    Ok(())
}
#[tokio::test]
async fn original_worktree_request_and_retained_output_reject_before_new_dynamic_copies(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let huge = "x".repeat(17 * 1024 * 1024);
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = counts(&runtime)?;
    let budget = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .create_worktree_for_workspace(
            &workspace.workspace_id,
            &huge,
            "HEAD",
            None,
            false,
            ExistingWorktreePolicy::Reuse,
        )
        .await
        .err()
        .context("huge branch copied before request gate")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        budget
    );
    let request = runtime.prepare_original_workspace_request_material(&"actual-retained-output")?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .prepare_original_registered_worktree_retained_observation(
            request.clone(),
            "original-root",
            &[huge],
        )
        .err()
        .context("huge changed-files JSON copied")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
