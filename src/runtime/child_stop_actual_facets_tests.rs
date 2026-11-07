//! Actual Host spawn and typed binding, with pure Stop candidates before/after.
use super::*;
use crate::runtime::{
    command_task::ManagedTaskHandle,
    control_cancellation::SharedControlTaskHandle,
    resident_task::{TaskCaptureRequest, TaskFamily, TaskKey},
    task_stop_intent::FrozenTaskStopIntent,
    RuntimeHandle,
};
use crate::runtime_db::task_stop::{TaskStopChildTarget, TaskStopProgress};
use crate::types::*;

fn git(path: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()?;
    ensure!(
        output.status.success(),
        "actual worktree fixture: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
async fn freeze(
    runtime: &RuntimeHandle,
    task: &TaskRecord,
    lease: &Arc<ControlJoinLease>,
    child: Option<&TaskStopChildTarget>,
) -> Result<FrozenTaskStopIntent> {
    let mut owner = runtime.begin_mutation().await?;
    let mut request = TaskCaptureRequest::current();
    request
        .required_keys
        .insert(TaskKey::new(TaskFamily::Task, &task.id));
    owner.prepare_owner_task_request(&request).await?;
    let selected = owner.capture_tasks(&request)?;
    let fact = selected
        .lookup(&TaskKey::new(TaskFamily::Task, &task.id))?
        .unwrap();
    let handle = ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone()));
    FrozenTaskStopIntent::prepare(
        fact,
        Some(&handle),
        runtime.inner.mutation_owner.owner_epoch(),
        child,
        runtime.now(),
        &runtime.inner.input_evidence_budget,
    )
}
#[tokio::test]
async fn actual_spawn_before_running_stop_must_cover_child_and_worktree_facets() -> Result<()> {
    for worktree in [false, true] {
        let (_home, host, runtime) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        if worktree {
            let repo = runtime.workspace_root();
            std::fs::create_dir_all(&repo)?;
            git(&repo, &["init", "--quiet"])?;
            git(
                &repo,
                &["config", "user.email", "original-monitor@localhost"],
            )?;
            git(&repo, &["config", "user.name", "Original Monitor"])?;
            std::fs::write(repo.join("tracked"), "original\n")?;
            git(&repo, &["add", "tracked"])?;
            git(&repo, &["commit", "--quiet", "-m", "original"])?;
        }
        let at = runtime.now();
        let task = TaskRecord {
            id: crate::ids::task_id(),
            agent_id: runtime.agent_state().await?.id,
            kind: if worktree {
                TaskKind::WorktreeSubagentTask
            } else {
                TaskKind::SubagentTask
            },
            status: TaskStatus::Running,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("actual spawn before Running".into()),
            detail: Some(
                serde_json::json!({"workspace_mode":if worktree{"worktree"}else{"inherit"}}),
            ),
            recovery: Some(TaskRecoverySpec::ChildAgentTask {
                summary: "actual spawn before Running".into(),
                prompt: "actual child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: if worktree {
                    ChildAgentWorkspaceMode::Worktree
                } else {
                    ChildAgentWorkspaceMode::Inherit
                },
                lifecycle_disposition: AgentLifecycleDisposition::Retain,
            }),
        };
        runtime
            .persist_task_transition(&task, "actual_stop_facets_admission")
            .await?;
        let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
        lease.attach(tokio::spawn(std::future::pending::<()>()));
        runtime.inner.task_handles.lock().await.insert(
            task.id.clone(),
            ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
        );
        drop(registration);
        let model = runtime.model_state_for(&runtime.agent_state().await?);
        let spawned = runtime
            .inner
            .host_bridge
            .as_ref()
            .unwrap()
            .spawn_child_task(
                runtime.clone(),
                &task,
                "actual spawned child".into(),
                AuthorityClass::OperatorInstruction,
                worktree,
                None,
                AgentModelResolution {
                    requested: None,
                    resolved_provider: model.effective_model.provider.as_str().into(),
                    resolved_model: model.effective_model.model,
                    resolved_parameters: None,
                    resolution_status: AgentModelResolutionStatus::Inherited,
                    policy_notes: vec![],
                },
            )
            .await?;
        source.retain_monitor_stage(
            task.clone(),
            AuthorityClass::OperatorInstruction,
            worktree,
            false,
            AgentLifecycleDisposition::Retain,
            spawned.child_agent_id.clone(),
            spawned.child_turn_baseline,
            spawned.delivery_id.clone(),
            spawned.task_detail,
        )?;
        runtime.bind_original_child_monitor_target(
            &source,
            &spawned.child_agent_id,
            spawned.delivery_id.as_deref(),
            spawned.child_turn_baseline,
        )?;
        let running_count:usize=runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM messages WHERE kind='task_status' AND json_extract(payload_json,'$.metadata.task_id')=?1",[&task.id],|r|r.get(0))?;
        assert_eq!(running_count, 0, "actual spawn has not emitted Running");
        let uncovered = freeze(&runtime, &task, &lease, None).await?;
        uncovered.intent.validate_original(&uncovered.task)?;
        let frozen_message = &uncovered.intent.result.as_ref().unwrap().message;
        let mut admitted_message = frozen_message.clone();
        admitted_message.normalize_admission_fields();
        assert_eq!(frozen_message, &admitted_message);
        assert_eq!(
            frozen_message.trigger_kind,
            Some(ContinuationTriggerKind::TaskResult)
        );
        assert_eq!(frozen_message.source_refs.get("task_id"), Some(&task.id));
        assert!(!source.original_stop_covers_actual_monitor_facets(
            &uncovered.intent,
            &uncovered.task,
            &TaskStopProgress::initial(&uncovered.intent)
        )?);
        assert!(source.has_terminal_responsibility() && source.monitor_stage_present());
        drop(uncovered);
        assert!(
            runtime
                .continue_original_child_monitor_binding(source.clone())
                .await?
        );
        let target = source.original_monitor_binding_target()?;
        let child = TaskStopChildTarget {
            scope: target.target_scope().unwrap().clone(),
            parent_agent_id: task.agent_id.clone(),
            creation_task_id: task.id.clone(),
        };
        let covered = freeze(&runtime, &task, &lease, Some(&child)).await?;
        covered.intent.validate_original(&covered.task)?;
        assert!(source.original_stop_covers_actual_monitor_facets(
            &covered.intent,
            &covered.task,
            &TaskStopProgress::initial(&covered.intent)
        )?);
        let mut forged = covered.intent.clone();
        forged.child.as_mut().unwrap().scope.identity_incarnation += 1;
        assert!(!source.original_stop_covers_actual_monitor_facets(
            &forged,
            &covered.task,
            &TaskStopProgress::initial(&forged)
        )?);
        if worktree {
            let artifact = covered.intent.worktree.as_ref().unwrap();
            assert!(std::path::Path::new(&artifact.path).exists());
            let mut forged = covered.intent.clone();
            forged.worktree.as_mut().unwrap().path.push_str("-other");
            assert!(!source.original_stop_covers_actual_monitor_facets(
                &forged,
                &covered.task,
                &TaskStopProgress::initial(&forged)
            )?);
        }
        drop(covered);
        drop(target);
        let stopped = runtime
            .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
            .await?;
        assert_eq!(stopped.status, TaskStatus::Cancelled);
        lease.join().await?;
        runtime
            .continue_original_child_terminal(source.clone())
            .await?;
        assert!(source.is_durable_finished() && !source.has_terminal_responsibility());
        runtime.join_owned_mutations().await?;
        host.shutdown().await?;
    }
    Ok(())
}
