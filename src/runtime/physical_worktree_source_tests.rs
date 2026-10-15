//! Source-gate model after a real Task ACK. The physical Host path remains an
//! existing integration fixture; these calls do not assert a synthetic FS ACK.
use super::*;
use crate::runtime::{
    tests::support::{context_config, tempdir},
    ManagedWorktreeSeed,
};
use crate::types::*;
#[tokio::test]
async fn original_child_worktree_target_gate_keeps_registered_creation_identity_and_exact_seed_distinct(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "physical-source-owner",
        home.path().into(),
        context_config(),
    )?;
    let at = runtime.now();
    let id = crate::ids::task_id();
    let task = TaskRecord {
        id: id.clone(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::WorktreeSubagentTask,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("source gate model".into()),
        detail: Some(
            serde_json::json!({"workspace_mode":"worktree","rejoin_obligation_id":id,"rejoin_generation":1}),
        ),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "source gate model".into(),
            prompt: "actual accepted source".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Worktree,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    runtime
        .persist_task_transition(&task, "original_physical_source_fixture")
        .await?;
    let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
    // Exercise the real control join contract even in this source-gate model.
    // Cancellation joins an actually attached pending observer.
    lease.attach(tokio::spawn(std::future::pending::<()>()));
    let model = runtime.model_state_for(&runtime.agent_state().await?);
    source.retain_original_child_spawn(
        task.clone(),
        "actual accepted source".into(),
        AuthorityClass::OperatorInstruction,
        true,
        false,
        AgentLifecycleDisposition::Retain,
        AgentModelResolution {
            requested: None,
            resolved_provider: model.effective_model.provider.as_str().into(),
            resolved_model: model.effective_model.model,
            resolved_parameters: None,
            resolution_status: AgentModelResolutionStatus::Inherited,
            policy_notes: vec![],
        },
        at,
    )?;
    let seed = ManagedWorktreeSeed {
        original_cwd: home.path().join("original"),
        original_branch: "main".into(),
        worktree_path: home.path().join("linked"),
        worktree_branch: "actual-linked".into(),
    };
    assert!(source
        .validate_original_spawn_worktree_target("original-child", 1, &seed)
        .is_err());
    source.claim_original_child_spawn_registration()?;
    source.original_spawn_worktree_planned(
        &seed.original_cwd,
        &seed.original_branch,
        &seed.worktree_path,
        &seed.worktree_branch,
    )?;
    // Model only the original Host's already-created identity/physical result
    // transitions, never use this fixture record to grant a DB or FS operation.
    let identity = AgentIdentityRecord::new(
        "original-child",
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(task.agent_id.clone()),
        Some(task.id.clone()),
    );
    source.original_spawn_identity_created(&identity)?;
    assert!(
        source
            .validate_original_spawn_worktree_target("original-child", 1, &seed)
            .is_err(),
        "planned differs from actual creation"
    );
    source.original_spawn_worktree_created(&seed.worktree_path, &seed.worktree_branch)?;
    source.validate_original_spawn_worktree_target("original-child", 1, &seed)?;
    assert!(source
        .validate_original_spawn_worktree_target("successor-child", 1, &seed)
        .is_err());
    assert!(source
        .validate_original_spawn_worktree_target("original-child", 2, &seed)
        .is_err());
    for field in 0..4 {
        let mut changed = seed.clone();
        match field {
            0 => changed.original_cwd = home.path().join("other-original"),
            1 => changed.original_branch = "other-main".into(),
            2 => changed.worktree_path = home.path().join("other-linked"),
            _ => changed.worktree_branch = "other-linked".into(),
        };
        assert!(
            source
                .validate_original_spawn_worktree_target("original-child", 1, &changed)
                .is_err(),
            "changed seed field {field}"
        );
    }
    source
        .record_original_child_spawn_completion(Err(anyhow::anyhow!("source-gate model ended")))?;
    assert!(
        source
            .validate_original_spawn_worktree_target("original-child", 1, &seed)
            .is_err(),
        "finished worker is not a new admission"
    );
    assert!(
        !seed.worktree_path.exists(),
        "source gate never performed filesystem work"
    );
    drop(registration);
    lease.cancel();
    lease.join().await?;
    Ok(())
}
