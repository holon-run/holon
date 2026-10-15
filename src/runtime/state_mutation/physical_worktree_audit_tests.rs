use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
    ManagedWorktreeSeed,
};
fn git(path: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "physical fixture Git: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, ManagedWorktreeSeed)> {
    let home = tempdir()?;
    let repo = home.path().join("physical-owner-repository");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Holon Test"])?;
    git(&repo, &["config", "user.email", "holon-test@example.com"])?;
    git(&repo, &["commit", "--allow-empty", "-m", "initial"])?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "physical-worktree-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let workspace = WorkspaceEntry::new("physical-owner-workspace", repo, None);
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
        .prepare_managed_worktree("actual-original-linked")
        .await?;
    Ok((home, runtime, seed))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),(SELECT COUNT(*) FROM current_read_members WHERE family='event'),(SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
fn events(runtime: &RuntimeHandle) -> Result<Vec<AuditEvent>> {
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root
        .current_reads
        .as_ref()
        .context("physical ACK omitted CurrentRead")?;
    let captured = current.capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    Ok(captured
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) => Some(e.clone()),
            _ => None,
        })
        .collect())
}
#[tokio::test]
async fn actual_legacy_physical_entry_and_exit_keep_two_original_acks_and_do_not_remove_git(
) -> Result<()> {
    for removed in [false, true] {
        let (_home, runtime, seed) = fixture().await?;
        let before = counts(&runtime)?;
        runtime
            .enter_worktree(
                seed.original_cwd.clone(),
                seed.original_branch.clone(),
                seed.worktree_path.clone(),
                seed.worktree_branch.clone(),
            )
            .await?;
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
        );
        let state = runtime.agent_state().await?;
        let session = state.worktree_session.as_ref().unwrap();
        assert_eq!(session.worktree_path, seed.worktree_path);
        assert_eq!(session.worktree_branch, seed.worktree_branch);
        let active = state.active_workspace_entry.as_ref().unwrap();
        assert_eq!(active.execution_root, seed.worktree_path);
        let root_id = active.execution_root_id.clone();
        let selected = events(&runtime)?;
        let event = selected
            .iter()
            .find(|e| e.kind == "worktree_entered")
            .unwrap();
        assert_eq!(
            event.data,
            serde_json::json!({"worktree":session,"boundary":crate::system::HostLocalBoundary::from_parts(&state.execution_profile,Some(WorkspaceProjectionKind::GitWorktreeRoot),Some(WorkspaceAccessMode::ExclusiveWrite),Some(root_id.clone())).audit_metadata()})
        );
        let previous = selected
            .iter()
            .filter(|e| e.kind == "agent_state_mutated")
            .max_by_key(|e| e.event_seq)
            .unwrap();
        assert!(previous.event_seq < event.event_seq);
        let before = counts(&runtime)?;
        if removed {
            git(
                &seed.original_cwd,
                &["worktree", "remove", seed.worktree_path.to_str().unwrap()],
            )?;
        }
        runtime
            .exit_worktree(seed.original_cwd.clone(), removed)
            .await?;
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
        );
        let after = runtime.agent_state().await?;
        assert!(after.worktree_session.is_none());
        assert_eq!(
            after
                .active_workspace_entry
                .as_ref()
                .unwrap()
                .execution_root,
            seed.original_cwd
        );
        assert_eq!(
            seed.worktree_path.exists(),
            !removed,
            "Runtime exit must not invent physical deletion"
        );
        let entry = runtime
            .runtime_db()
            .execution_root_entries()
            .get(&root_id)?
            .unwrap();
        assert_eq!(entry.removed_at.is_some(), removed);
        git(
            &seed.original_cwd,
            &[
                "show-ref",
                "--verify",
                &format!("refs/heads/{}", seed.worktree_branch),
            ],
        )?;
        let selected = events(&runtime)?;
        let exited = selected
            .iter()
            .find(|e| e.kind == "worktree_exited")
            .unwrap();
        assert_eq!(
            exited.data,
            serde_json::json!({"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch,"removed":removed})
        );
        assert!(
            selected
                .iter()
                .filter(|e| e.kind == "agent_state_mutated")
                .max_by_key(|e| e.event_seq)
                .unwrap()
                .event_seq
                < exited.event_seq
        );
        let _pause = runtime.pause_resource_observer_for_test().await;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO forbidden_physical_events;ALTER TABLE agent_states RENAME TO forbidden_physical_states;")?;
        assert!(root
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==exited.id)));
    }
    Ok(())
}
#[tokio::test]
async fn accepted_legacy_entry_and_exit_complete_after_caller_abort_and_closing() -> Result<()> {
    for exit in [false, true] {
        let (_home, runtime, seed) = fixture().await?;
        if exit {
            runtime.enter_worktree_from_seed(&seed).await?;
        }
        let owner = runtime.begin_mutation().await?;
        let before = counts(&runtime)?;
        let n = runtime
            .inner
            .mutation_tasks
            .lock()
            .unwrap()
            .handles
            .iter()
            .filter(|t| !t.is_finished())
            .count();
        let caller = tokio::spawn({
            let runtime = runtime.clone();
            let seed = seed.clone();
            async move {
                if exit {
                    runtime.exit_worktree(seed.original_cwd, false).await
                } else {
                    runtime.enter_worktree_from_seed(&seed).await
                }
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
                .filter(|t| !t.is_finished())
                .count()
                <= n
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("physical request was not actually registered")?;
        runtime.close_mutation_admission();
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        assert!(runtime.enter_worktree_from_seed(&seed).await.is_err());
        drop(owner);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.join_owned_mutations(),
        )
        .await??;
        let state = runtime.agent_state().await?;
        assert_eq!(state.worktree_session.is_some(), !exit);
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
        );
        assert!(seed.worktree_path.join(".git").is_file());
    }
    Ok(())
}
#[tokio::test]
async fn legacy_worktree_state_fault_keeps_original_physical_result_without_fabricating_followup_audit(
) -> Result<()> {
    for mode in ["rollback", "known"] {
        let (_home, runtime, seed) = fixture().await?;
        let state = runtime.agent_state().await?;
        let before = counts(&runtime)?;
        runtime.inject_next_transition_fault(if mode == "rollback" {
            TransitionFaultPoint::AfterAuditWrites
        } else {
            TransitionFaultPoint::BeforeCacheUpdate
        });
        assert!(runtime.enter_worktree_from_seed(&seed).await.is_err());
        assert!(
            seed.worktree_path.join(".git").is_file(),
            "entry never acquired physical cleanup authority"
        );
        if mode == "rollback" {
            assert_eq!(runtime.agent_state().await?, state);
            assert_eq!(counts(&runtime)?, before);
        } else {
            assert!(matches!(
                runtime.inner.mutation_owner.capture(),
                Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
            ));
            drop(runtime.begin_mutation().await?);
            assert_eq!(
                runtime
                    .agent_state()
                    .await?
                    .worktree_session
                    .unwrap()
                    .worktree_path,
                seed.worktree_path
            );
            assert_eq!(
                counts(&runtime)?,
                (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
            );
        }
        assert!(!events(&runtime)?
            .iter()
            .any(|e| e.kind == "worktree_entered"));
    }
    Ok(())
}
#[tokio::test]
async fn borrowed_legacy_worktree_request_rejects_before_copy_state_and_git() -> Result<()> {
    let (_home, runtime, mut seed) = fixture().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    seed.worktree_branch = "x".repeat(17 * 1024 * 1024);
    let before = counts(&runtime)?;
    let state = runtime.agent_state().await?;
    let used = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .enter_worktree_from_seed(&seed)
        .await
        .err()
        .context("oversized borrowed seed was copied")?;
    assert!(error.downcast_ref::<CurrentReadError>().is_some());
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(runtime.agent_state().await?, state);
    assert!(seed.worktree_path.join(".git").is_file());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        used
    );
    Ok(())
}
