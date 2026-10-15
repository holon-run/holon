use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};
fn git(path: &Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "parent fixture Git: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, TaskRecord)> {
    let home = tempdir()?;
    let repo = home.path().join("parent-physical-repository");
    std::fs::create_dir(&repo)?;
    git(&repo, &["init", "-b", "main"])?;
    git(&repo, &["config", "user.name", "Holon Test"])?;
    git(&repo, &["config", "user.email", "holon-test@example.com"])?;
    git(&repo, &["commit", "--allow-empty", "-m", "initial"])?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "physical-parent-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let workspace = WorkspaceEntry::new("physical-parent-workspace", repo, None);
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
    let at = runtime.now();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::WorktreeSubagentTask,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("actual parent physical task".into()),
        detail: Some(serde_json::json!({"workspace_mode":"worktree"})),
        recovery: None,
    };
    runtime
        .persist_task_transition(&task, "physical_parent_initial_actual_ack")
        .await?;
    Ok((home, runtime, task))
}
fn events(runtime: &RuntimeHandle) -> Result<Vec<AuditEvent>> {
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    Ok(root
        .current_reads
        .as_ref()
        .context("parent original ACK lost Current")?
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
async fn actual_parent_git_creation_task_metadata_and_two_observations_preserve_original_order(
) -> Result<()> {
    let (_home, runtime, task) = fixture().await?;
    let seed = runtime.prepare_managed_worktree_for_task(&task.id).await?;
    assert!(seed.worktree_path.join(".git").is_file());
    let stored = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
    assert_eq!(stored.status, TaskStatus::Running);
    assert_eq!(stored.created_at, task.created_at);
    assert_eq!(stored.parent_message_id, task.parent_message_id);
    assert_eq!(
        stored.detail.as_ref().unwrap()["worktree"],
        serde_json::json!({"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch,"projection_kind":"git_worktree_root","original_cwd":seed.original_cwd,"original_branch":seed.original_branch})
    );
    let all = events(&runtime)?;
    let created = all
        .iter()
        .filter(|e| e.kind == "worktree_created_for_task")
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 1);
    let recorded = all
        .iter()
        .filter(|e| e.kind == "task_worktree_metadata_recorded")
        .collect::<Vec<_>>();
    assert_eq!(recorded.len(), 1);
    let changed = all
        .iter()
        .filter(|e| e.kind == "task_status_updated" && e.data["task_id"] == task.id)
        .max_by_key(|e| e.event_seq)
        .unwrap();
    assert!(created[0].event_seq < changed.event_seq && changed.event_seq < recorded[0].event_seq);
    assert_eq!(
        created[0].data,
        serde_json::json!({"task_id":task.id,"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch,"original_cwd":seed.original_cwd,"original_branch":seed.original_branch})
    );
    assert_eq!(
        recorded[0].data,
        serde_json::json!({"task_id":task.id,"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch})
    );
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM current_event_native_births WHERE event_id IN (?1,?2)",
            rusqlite::params![created[0].id, recorded[0].id],
            |r| r.get::<_, usize>(0)
        )?,
        2
    );
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.runtime_db().connection()?.execute_batch("ALTER TABLE audit_events RENAME TO forbidden_parent_physical_events;ALTER TABLE tasks RENAME TO forbidden_parent_physical_tasks;ALTER TABLE agent_states RENAME TO forbidden_parent_physical_states;")?;
    assert!(root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
        .events
        .iter()
        .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==recorded[0].id)));
    Ok(())
}
#[tokio::test]
async fn accepted_parent_physical_request_keeps_task_and_audits_after_caller_abort_and_closing(
) -> Result<()> {
    let (_home, runtime, task) = fixture().await?;
    let owner = runtime.begin_mutation().await?;
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
        let id = task.id.clone();
        async move { runtime.prepare_managed_worktree_for_task(&id).await }
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
    .await?;
    runtime.close_mutation_admission();
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    assert!(runtime
        .prepare_managed_worktree_for_task(&task.id)
        .await
        .is_err());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    let stored = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
    assert_eq!(stored.status, TaskStatus::Running);
    let path = stored.detail.as_ref().unwrap()["worktree"]["worktree_path"]
        .as_str()
        .unwrap();
    assert!(Path::new(path).join(".git").is_file());
    let all = events(&runtime)?;
    for kind in [
        "worktree_created_for_task",
        "task_worktree_metadata_recorded",
    ] {
        assert_eq!(all.iter().filter(|e| e.kind == kind).count(), 1);
    }
    Ok(())
}
#[tokio::test]
async fn parent_observation_failure_keeps_actual_git_result_and_original_task_ack_boundary(
) -> Result<()> {
    for kind in [
        "worktree_created_for_task",
        "task_worktree_metadata_recorded",
    ] {
        let (_home, runtime, task) = fixture().await?;
        let repo = runtime.workspace_root();
        runtime.runtime_db().connection()?.execute_batch(&format!("CREATE TRIGGER original_parent_physical_audit_fault BEFORE INSERT ON audit_events WHEN NEW.kind='{kind}' BEGIN SELECT RAISE(ABORT,'actual original parent audit failure'); END;"))?;
        assert!(runtime
            .prepare_managed_worktree_for_task(&task.id)
            .await
            .is_err());
        let path = repo
            .parent()
            .unwrap()
            .join(".holon-worktrees-parent-physical-repository")
            .join(format!("task-{}", task.id));
        assert!(path.join(".git").is_file());
        let stored = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
        if kind == "worktree_created_for_task" {
            assert_eq!(stored, task);
        } else {
            assert_eq!(stored.status, TaskStatus::Running);
            assert!(stored.detail.as_ref().unwrap().get("worktree").is_some());
        }
        assert!(!events(&runtime)?.iter().any(|e| e.kind == kind));
        runtime
            .runtime_db()
            .connection()?
            .execute_batch("DROP TRIGGER original_parent_physical_audit_fault;")?;
        assert!(
            runtime
                .prepare_managed_worktree_for_task(&task.id)
                .await
                .is_err(),
            "existing physical result cannot cause a second creation"
        );
        assert!(path.exists());
    }
    Ok(())
}
#[tokio::test]
async fn compound_workspace_task_copy_borrows_actual_guard_once_and_releases_its_last_holder(
) -> Result<()> {
    let (_home, runtime, task) = fixture().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let owner = runtime.begin_mutation().await?;
    let scope = owner.input_evidence_scope();
    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
        query: Default::default(),
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let original = runtime.prepare_original_workspace_request_material(&task.id)?;
    let base = original.material_estimate()?;
    let copy = crate::runtime_db::command_task_terminal::charge_material(
        &scope,
        4096,
        64,
        &request,
        &runtime.inner.input_evidence_budget,
    )?;
    let copy_weak = Arc::downgrade(&copy);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let compound = original.with_original_task_copy(copy, 64)?;
    assert_eq!(compound.material_estimate()?, (base.0 + 4096, base.1 + 64));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before,
        "borrowing existing copy cannot register it twice"
    );
    let mut visited = Vec::new();
    compound.visit_actual_guards(|guard| {
        visited.push(Arc::as_ptr(guard) as usize);
        Ok(())
    })?;
    assert_eq!(visited.len(), 2);
    assert_ne!(visited[0], visited[1]);
    compound.validate_binding(
        &scope,
        runtime.instance_owner_epoch(),
        &runtime.inner.input_evidence_budget,
    )?;
    let extra = crate::runtime_db::command_task_terminal::charge_material(
        &scope,
        128,
        1,
        &request,
        &runtime.inner.input_evidence_budget,
    )?;
    assert!(compound.with_original_task_copy(extra, 1).is_err());
    assert!(copy_weak.upgrade().is_some());
    drop(compound);
    assert!(copy_weak.upgrade().is_none());
    drop((original, owner));
    Ok(())
}

#[tokio::test]
async fn actual_scalar_task_detail_keeps_original_worktree_metadata_branch_and_observation(
) -> Result<()> {
    let (_home, runtime, original) = fixture().await?;
    let task = TaskRecord {
        id: crate::ids::task_id(),
        detail: Some(serde_json::json!("original scalar task detail")),
        ..original
    };
    runtime
        .persist_task_transition(&task, "physical_scalar_actual_initial_ack")
        .await?;
    assert_eq!(
        runtime
            .runtime_db()
            .tasks()
            .latest(&task.id)?
            .unwrap()
            .detail,
        task.detail
    );
    let seed = runtime.prepare_managed_worktree_for_task(&task.id).await?;
    assert!(seed.worktree_path.join(".git").is_file());
    let stored = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
    assert_eq!(stored.status, TaskStatus::Running);
    assert_eq!(
        stored.detail, task.detail,
        "original as_object_mut branch leaves scalar detail intact"
    );
    assert_eq!(stored.created_at, task.created_at);
    let all = events(&runtime)?;
    let created = all
        .iter()
        .filter(|e| e.kind == "worktree_created_for_task" && e.data["task_id"] == task.id)
        .collect::<Vec<_>>();
    let changed = all
        .iter()
        .filter(|e| e.kind == "task_status_updated" && e.data["task_id"] == task.id)
        .max_by_key(|e| e.event_seq)
        .unwrap();
    let recorded = all
        .iter()
        .filter(|e| e.kind == "task_worktree_metadata_recorded" && e.data["task_id"] == task.id)
        .collect::<Vec<_>>();
    assert_eq!(created.len(), 1);
    assert_eq!(recorded.len(), 1);
    assert!(created[0].event_seq < changed.event_seq && changed.event_seq < recorded[0].event_seq);
    assert_eq!(
        recorded[0].data,
        serde_json::json!({"task_id":task.id,"worktree_path":seed.worktree_path,"worktree_branch":seed.worktree_branch})
    );
    Ok(())
}
