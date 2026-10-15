use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};
use crate::types::WorkspaceProjectionMetadata;
fn git(path: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "temporary fixture git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, WorkspaceEntry)> {
    let home = tempdir()?;
    let repo = home.path().join("original-enter-repository");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Holon Test"])?;
    git(&repo, &["config", "user.email", "holon-test@example.com"])?;
    git(&repo, &["commit", "--allow-empty", "-m", "initial"])?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "workspace-enter-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let workspace = WorkspaceEntry::new("original-enter-workspace", repo, None);
    runtime.attach_workspace(&workspace).await?;
    Ok((home, runtime, workspace))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),
        (SELECT COUNT(*) FROM current_read_members WHERE family='event'),
        (SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
fn entered(runtime: &RuntimeHandle) -> Result<AuditEvent> {
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let captured = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    captured
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind == "workspace_entered" => Some(e),
            _ => None,
        })
        .max_by_key(|e| e.event_seq)
        .cloned()
        .context("original entered event missing")
}
#[tokio::test]
async fn actual_canonical_workspace_entry_keeps_state_then_original_observation_and_repeat_noop(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let before = counts(&runtime)?;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            None,
            None,
        )
        .await?;
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
    );
    let state = runtime.agent_state().await?;
    let active = state.active_workspace_entry.as_ref().unwrap();
    assert_eq!(active.execution_root, workspace.workspace_anchor);
    assert_eq!(active.access_mode, WorkspaceAccessMode::SharedRead);
    assert!(state.worktree_session.is_none());
    let event = entered(&runtime)?;
    assert_eq!(
        event.data,
        serde_json::json!({
            "workspace_id":workspace.workspace_id,"workspace_anchor":workspace.workspace_anchor,
            "execution_root_id":active.execution_root_id,"execution_root":active.execution_root,
            "projection_kind":active.projection_kind,"access_mode":active.access_mode,"cwd":active.cwd,
            "boundary":crate::system::HostLocalBoundary::from_parts(&state.execution_profile,Some(active.projection_kind),Some(active.access_mode),Some(active.execution_root_id.clone())).audit_metadata()
        })
    );
    let before = counts(&runtime)?;
    runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::CanonicalRoot,
            WorkspaceAccessMode::SharedRead,
            None,
            None,
        )
        .await?;
    assert_eq!(runtime.agent_state().await?, state);
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let capture = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_enter_events;
        ALTER TABLE agent_states RENAME TO forbidden_enter_states;",
    )?;
    assert_eq!(
        root.current_reads
            .as_ref()
            .unwrap()
            .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
            .events
            .len(),
        capture.events.len()
    );
    Ok(())
}
#[tokio::test]
async fn actual_managed_and_existing_worktree_entries_keep_original_metadata_and_physical_roots(
) -> Result<()> {
    for mode in ["managed", "existing"] {
        let (home, runtime, workspace) = fixture().await?;
        runtime
            .enter_workspace(
                &workspace,
                WorkspaceProjectionKind::CanonicalRoot,
                WorkspaceAccessMode::SharedRead,
                None,
                None,
            )
            .await?;
        let linked = home.path().join("external-linked-root");
        if mode == "existing" {
            git(
                &workspace.workspace_anchor,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "original-linked",
                    linked.to_str().unwrap(),
                ],
            )?;
        }
        let before = counts(&runtime)?;
        if mode == "managed" {
            runtime
                .enter_workspace(
                    &workspace,
                    WorkspaceProjectionKind::GitWorktreeRoot,
                    WorkspaceAccessMode::ExclusiveWrite,
                    None,
                    Some("original-managed".into()),
                )
                .await?;
        } else {
            runtime
                .enter_existing_git_worktree(
                    &workspace,
                    linked.clone(),
                    WorkspaceAccessMode::ExclusiveWrite,
                    None,
                )
                .await?;
        }
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2),
            "mode {mode}"
        );
        let state = runtime.agent_state().await?;
        let active = state.active_workspace_entry.as_ref().unwrap();
        assert!(active.execution_root.join(".git").is_file());
        assert_eq!(
            active.projection_kind,
            WorkspaceProjectionKind::GitWorktreeRoot
        );
        assert_eq!(active.access_mode, WorkspaceAccessMode::ExclusiveWrite);
        assert!(
            active.occupancy_id.is_none(),
            "standalone fixture does not mint Host occupancy"
        );
        let registered = runtime
            .runtime_db()
            .execution_root_entries()
            .get(&active.execution_root_id)?
            .unwrap();
        assert_eq!(registered.filesystem_path, active.execution_root);
        assert!(registered.removed_at.is_none());
        let event = entered(&runtime)?;
        assert_eq!(event.data["execution_root_id"], active.execution_root_id);
        assert_eq!(
            event.data["boundary"]["execution_root_id"],
            active.execution_root_id
        );
        assert_eq!(event.data["cwd"], serde_json::to_value(&active.cwd)?);
        if mode == "existing" {
            assert!(state.worktree_session.is_none());
            assert!(matches!(
                active.projection_metadata,
                Some(WorkspaceProjectionMetadata::ExistingGitWorktree(_))
            ));
            assert_eq!(active.execution_root, linked);
            assert_eq!(event.data["detected_kind"], "existing_git_worktree");
            assert_eq!(event.data["ownership"], "external");
        } else {
            assert_eq!(
                state.worktree_session.as_ref().unwrap().worktree_path,
                active.execution_root
            );
            assert!(matches!(
                active.projection_metadata,
                Some(WorkspaceProjectionMetadata::ManagedWorktree(_))
            ));
            assert!(event.data.get("detected_kind").is_none());
            assert!(event.data.get("ownership").is_none());
        }
    }
    Ok(())
}
#[tokio::test]
async fn original_managed_entry_state_rollback_discards_only_new_worktree_and_known_ack_keeps_it(
) -> Result<()> {
    for mode in ["rollback", "known"] {
        let (_home, runtime, workspace) = fixture().await?;
        runtime
            .enter_workspace(
                &workspace,
                WorkspaceProjectionKind::CanonicalRoot,
                WorkspaceAccessMode::SharedRead,
                None,
                None,
            )
            .await?;
        let before = runtime.agent_state().await?;
        let baseline = counts(&runtime)?;
        let expected = workspace
            .workspace_anchor
            .parent()
            .unwrap()
            .join(".holon-worktrees-original-enter-repository")
            .join("original-entry-fault");
        runtime.inject_next_transition_fault(if mode == "rollback" {
            TransitionFaultPoint::AfterAuditWrites
        } else {
            TransitionFaultPoint::BeforeCacheUpdate
        });
        let result = runtime
            .enter_workspace(
                &workspace,
                WorkspaceProjectionKind::GitWorktreeRoot,
                WorkspaceAccessMode::ExclusiveWrite,
                None,
                Some("original-entry-fault".into()),
            )
            .await;
        if mode == "rollback" {
            let error = result
                .err()
                .context("managed State rollback did not fire")?;
            assert_eq!(
                crate::runtime_db::persistence_certainty(&error),
                Some(crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted)
            );
            assert_eq!(runtime.agent_state().await?, before);
            assert_eq!(counts(&runtime)?, baseline);
            assert!(
                !expected.exists(),
                "only DNC may discard the newly made physical worktree"
            );
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(&workspace.workspace_anchor)
                .args([
                    "show-ref",
                    "--verify",
                    "--quiet",
                    "refs/heads/original-entry-fault",
                ])
                .status()?;
            assert!(!status.success());
        } else {
            result?;
            let state = runtime.agent_state().await?;
            let active = state.active_workspace_entry.unwrap();
            assert_eq!(active.execution_root, expected);
            assert!(expected.join(".git").is_file());
            assert_eq!(
                counts(&runtime)?,
                (
                    baseline.0 + 2,
                    baseline.1 + 2,
                    baseline.2 + 2,
                    baseline.3 + 2
                )
            );
            assert!(runtime.inner.mutation_owner.capture().is_ok());
        }
        assert!(workspace.workspace_anchor.join(".git").exists());
    }
    Ok(())
}
#[tokio::test]
async fn accepted_canonical_and_existing_entry_finish_after_caller_abort_without_reopening_admission(
) -> Result<()> {
    for mode in ["canonical", "existing"] {
        let (home, runtime, workspace) = fixture().await?;
        let linked = home.path().join("cancelled-caller-linked");
        if mode == "existing" {
            git(
                &workspace.workspace_anchor,
                &[
                    "worktree",
                    "add",
                    "-b",
                    "cancelled-linked",
                    linked.to_str().unwrap(),
                ],
            )?;
        }
        let owner = runtime.begin_mutation().await?;
        let before = counts(&runtime)?;
        // Registration reaps completed setup workers. Compare only actual
        // live handles on both sides, while this owner keeps the new one busy.
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
            let workspace = workspace.clone();
            let linked = linked.clone();
            async move {
                if mode == "existing" {
                    runtime
                        .enter_existing_git_worktree(
                            &workspace,
                            linked,
                            WorkspaceAccessMode::ExclusiveWrite,
                            None,
                        )
                        .await
                } else {
                    runtime
                        .enter_workspace(
                            &workspace,
                            WorkspaceProjectionKind::CanonicalRoot,
                            WorkspaceAccessMode::SharedRead,
                            None,
                            None,
                        )
                        .await
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
            .enter_workspace(
                &workspace,
                WorkspaceProjectionKind::CanonicalRoot,
                WorkspaceAccessMode::SharedRead,
                None,
                None
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
        let active = state.active_workspace_entry.unwrap();
        assert_eq!(
            active.execution_root,
            if mode == "existing" {
                linked
            } else {
                workspace.workspace_anchor
            }
        );
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
        );
    }
    Ok(())
}
#[tokio::test]
async fn borrowed_entry_request_rejects_before_workspace_copy_or_git_side_effects() -> Result<()> {
    let (_home, runtime, mut workspace) = fixture().await?;
    workspace.repo_name = Some("x".repeat(17 * 1024 * 1024));
    // This assertion measures only the rejected request. Join the unrelated
    // background refresher, then finish its real foreground observation before
    // taking a stable baseline; no business write runs under the pause.
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = counts(&runtime)?;
    let budget = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .enter_workspace(
            &workspace,
            WorkspaceProjectionKind::GitWorktreeRoot,
            WorkspaceAccessMode::ExclusiveWrite,
            None,
            Some("must-not-create".into()),
        )
        .await
        .err()
        .context("oversized entry copied")?;
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
    assert!(!workspace
        .workspace_anchor
        .parent()
        .unwrap()
        .join(".holon-worktrees-original-enter-repository/must-not-create")
        .exists());
    Ok(())
}
