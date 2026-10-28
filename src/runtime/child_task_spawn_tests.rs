//! A real Host identity exists while the abortable original monitor is gone.
use super::*;
use crate::runtime::{
    command_task::ManagedTaskHandle, control_cancellation::SharedControlTaskHandle,
};
use crate::types::*;
use std::time::Duration;

#[tokio::test]
async fn registered_host_spawn_keeps_success_and_failure_after_original_monitor_abort() -> Result<()>
{
    for mode in [0, 1, 2, 3] {
        for before_first_poll in [false, true] {
            let fail_worktree = mode > 0;
            let (_home, host, runtime) =
                crate::runtime::tests::support::host_backed_test_runtime().await;
            if mode >= 2 {
                let repo = runtime.workspace_root();
                std::fs::create_dir_all(&repo)?;
                fn git(path: &std::path::Path, args: &[&str]) -> Result<()> {
                    let output = std::process::Command::new("git")
                        .args(args)
                        .current_dir(path)
                        .output()?;
                    ensure!(
                        output.status.success(),
                        "owned spawn Git fixture: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                    Ok(())
                }
                git(&repo, &["init", "--quiet"])?;
                git(&repo, &["config", "user.email", "owned-spawn@localhost"])?;
                git(&repo, &["config", "user.name", "Owned Spawn"])?;
                std::fs::write(repo.join("tracked"), "original\n")?;
                git(&repo, &["add", "tracked"])?;
                git(&repo, &["commit", "--quiet", "-m", "original"])?;
                if mode == 2 {
                    runtime.runtime_db().transaction(|tx| {tx.execute_batch("CREATE TRIGGER owned_spawn_worktree_audit_fail BEFORE INSERT ON audit_events WHEN NEW.kind='worktree_created_for_task' BEGIN SELECT RAISE(ABORT,'actual created artifact audit failure'); END;")?;Ok(())})?;
                }
            }
            let at = runtime.now();
            let id = crate::ids::task_id();
            let task = TaskRecord {
                id: id.clone(),
                agent_id: runtime.agent_state().await?.id,
                kind: if fail_worktree {
                    TaskKind::WorktreeSubagentTask
                } else {
                    TaskKind::SubagentTask
                },
                status: TaskStatus::Queued,
                created_at: at,
                updated_at: at,
                parent_message_id: None,
                work_item_id: None,
                summary: Some("registered finite Host spawn".into()),
                detail: Some(
                    serde_json::json!({"workspace_mode":if fail_worktree{"worktree"}else{"inherit"},"rejoin_obligation_id":id,"rejoin_generation":1}),
                ),
                recovery: Some(TaskRecoverySpec::ChildAgentTask {
                    summary: "registered finite Host spawn".into(),
                    prompt: "actual accepted child".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                    workspace_mode: if fail_worktree {
                        ChildAgentWorkspaceMode::Worktree
                    } else {
                        ChildAgentWorkspaceMode::Inherit
                    },
                    lifecycle_disposition: AgentLifecycleDisposition::Retain,
                }),
            };
            runtime
                .persist_task_transition(&task, "owned_spawn_fixture_admitted")
                .await?;
            let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
            let model = runtime.model_state_for(&runtime.agent_state().await?);
            source.retain_original_child_spawn(
                task.clone(),
                "actual accepted child".into(),
                AuthorityClass::OperatorInstruction,
                fail_worktree,
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
            let completion_weak = Arc::downgrade(&source.completion_material()?.guard());
            let gate = Arc::new(tokio::sync::Semaphore::new(0));
            source.set_original_spawn_identity_gate_for_test(gate.clone())?;
            if mode == 3 {
                source.set_original_spawn_enqueue_fault_for_test(
                    TransitionFaultPoint::BeforeCacheUpdate,
                )?;
            }
            runtime
                .inner
                .host_bridge
                .as_ref()
                .unwrap()
                .start_original_child_spawn(runtime.clone(), source.clone())
                .await?;
            let monitor = runtime.clone();
            let original = source.clone();
            let (_before_poll_send, before_poll_recv) = tokio::sync::oneshot::channel::<()>();
            lease.attach(tokio::spawn(async move {
                if before_first_poll {
                    let _ = before_poll_recv.await;
                } else {
                    drop(before_poll_recv);
                }
                let _ = monitor.continue_original_child_spawn(original).await;
            }));
            runtime.inner.task_handles.lock().await.insert(
                task.id.clone(),
                ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
            );
            drop(registration);
            let created = tokio::time::timeout(
                Duration::from_secs(10),
                source.wait_original_spawn_identity_for_test(),
            )
            .await??;
            assert_eq!(created.1, 1);
            let child_dispatch = if mode == 3 {
                let dispatch = Arc::new(tokio::sync::Semaphore::new(0));
                host.set_clock_recovery_dispatch_gate_for_test(&created.0, dispatch.clone());
                Some(dispatch)
            } else {
                None
            };
            let actual:bool=runtime.runtime_db().connection()?.query_row("SELECT EXISTS(SELECT 1 FROM agent_identities WHERE agent_id=?1 AND json_extract(payload_json,'$.parent_agent_id')=?2 AND json_extract(payload_json,'$.delegated_from_task_id')=?3)",rusqlite::params![created.0,task.agent_id,task.id],|r|r.get(0))?;
            assert!(actual);
            assert!(source.monitor_stage().is_none() && source.observed_terminal().is_none());
            lease.cancel();
            lease.join().await?;
            assert!(source.has_terminal_responsibility() && source.original_child_spawn_pending());
            assert!(runtime
                .inner
                .control_cancellations
                .contains_original_child_lease_for_test(&lease));
            // The Host-owned kernel remains registered; abort affected only the
            // observer, and the exact source has not acquired a replacement handle.
            gate.add_permits(1);
            tokio::time::timeout(
                Duration::from_secs(15),
                runtime.continue_original_child_spawn(source.clone()),
            )
            .await??;
            assert!(!source.original_child_spawn_pending());
            if fail_worktree {
                let observed = source
                    .observed_terminal()
                    .context("real Git failure was not retained")?;
                assert_eq!(observed.task.status, TaskStatus::Failed);
                assert_eq!(observed.child_id.as_deref(), Some(created.0.as_str()));
                assert!(source.failed_spawn().is_some());
                let original_scope = source
                    .original_created_spawn_scope()?
                    .context("actual created child incarnation was dropped with spawn capsule")?;
                assert_eq!(original_scope.agent_id, created.0);
                assert_eq!(original_scope.identity_incarnation, created.1);
                assert_eq!(
                    observed.task.detail.as_ref().unwrap()["target_identity_incarnation"],
                    created.1
                );
                if mode == 1 && !before_first_poll {
                    // Adversarial normalized/raw identity drift, not a public
                    // Host recreation path. No new scope proof is minted here.
                    let request = crate::runtime_db::task_snapshot::TaskDomainLoadRequest {
                        recovery_at: None,
                        query: Default::default(),
                        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                        record_limit: 65_536,
                        byte_limit: 64 * 1024 * 1024,
                    };
                    let failure = source.failed_spawn().unwrap();
                    runtime.runtime_db().transaction(|tx| {
                        let original:String=tx.query_row("SELECT payload_json FROM agent_identities WHERE agent_id=?1",[&created.0],|row|row.get(0))?;
                        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',?2) WHERE agent_id=?1",rusqlite::params![created.0,created.1+1])?;
                        let error=crate::runtime_db::child_task_terminal::bind_failed_original_child_target_tx(tx,&source,&failure,&request,&runtime.inner.input_evidence_budget).err().context("failed source adopted a successor incarnation")?;
                        assert!(format!("{error:#}").contains("original created incarnation"));
                        tx.execute("UPDATE agent_identities SET payload_json=?2 WHERE agent_id=?1",rusqlite::params![created.0,original])?;
                        Ok(())
                    })?;
                    assert!(source.source_view()?.target().is_none());
                    assert!(!source.is_durable_finished());
                }
                let artifact_path = if mode >= 2 {
                    let detail = observed.task.detail.as_ref().unwrap();
                    assert_eq!(detail["worktree_setup_confirmed"], true);
                    let path = detail["worktree"]["worktree_path"].as_str().unwrap();
                    assert!(
                        std::path::Path::new(path).exists(),
                        "actual Git result survived its failed audit before terminal cleanup"
                    );
                    assert!(
                        !observed.worktree,
                        "spawn failure cannot mint child exit authority"
                    );
                    assert_eq!(detail["worktree"]["cleanup_status"], "retained");
                    assert_eq!(
                        detail["worktree"]["cleanup_reason"],
                        "original_child_exit_unproven"
                    );
                    Some(std::path::PathBuf::from(path))
                } else {
                    None
                };
                drop(observed);
                runtime
                    .continue_original_child_terminal(source.clone())
                    .await?;
                assert_eq!(
                    runtime
                        .runtime_db()
                        .tasks()
                        .latest(&task.id)?
                        .unwrap()
                        .status,
                    TaskStatus::Failed
                );
                if let Some(path) = artifact_path {
                    assert!(
                        path.exists(),
                        "parent terminal wholeACK must retain unjoined child artifact"
                    );
                    let stored = runtime.runtime_db().tasks().latest(&task.id)?.unwrap();
                    assert_eq!(
                        stored.detail.as_ref().unwrap()["worktree"]["cleanup_status"],
                        "retained"
                    );
                    if mode == 3 {
                        // The same actual child Queue ACK survived its publication
                        // error. Reconcile that owner, then let the real child run.
                        let child = host
                            .get_agent_for_local_status(&created.0)
                            .await
                            .map_err(anyhow::Error::from)?;
                        assert_eq!(child.instance_incarnation(), created.1);
                        {
                            let mutation = child.begin_mutation().await?;
                            assert!(mutation.state.total_message_count > 0);
                        }
                        let prompt_id:String=runtime.runtime_db().connection()?.query_row("SELECT evidence_id FROM messages WHERE agent_id=?1 AND json_extract(payload_json,'$.metadata.delegated_task_id')=?2",rusqlite::params![created.0,task.id],|r|r.get(0))?;
                        child_dispatch.as_ref().unwrap().add_permits(1024);
                        child.inner.notify.notify_one();
                        tokio::time::timeout(Duration::from_secs(15), async {
                            loop {
                                if runtime
                                    .runtime_db()
                                    .queue_entries()
                                    .latest(&prompt_id)?
                                    .is_some_and(|queue| {
                                        queue.status == QueueEntryStatus::Processed
                                    })
                                {
                                    break Ok::<(), anyhow::Error>(());
                                }
                                tokio::task::yield_now().await;
                            }
                        })
                        .await??;
                        assert!(
                            path.exists(),
                            "committed child prompt still owns its working directory"
                        );
                    }
                }
            } else {
                let stage = source
                    .monitor_stage()
                    .context("actual spawn success lost after abort")?;
                assert_eq!(stage.child, created.0);
                assert_eq!(stage.detail["target_identity_incarnation"], created.1);
                drop(stage);
                assert!(
                    runtime
                        .continue_original_child_monitor_binding(source.clone())
                        .await?
                );
                let stopped = runtime
                    .stop_task(&task.id, &AuthorityClass::OperatorInstruction)
                    .await?;
                assert_eq!(stopped.status, TaskStatus::Cancelled);
                runtime
                    .continue_original_child_terminal(source.clone())
                    .await?;
            }
            assert!(source.is_durable_finished());
            assert!(source.original_created_spawn_scope()?.is_none());
            assert!(!source.has_terminal_responsibility());
            assert!(
                completion_weak.upgrade().is_none(),
                "confirmed whole ACK releases the finite-spawn body guard"
            );
            let children:usize=runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM agent_identities WHERE json_extract(payload_json,'$.parent_agent_id')=?1 AND json_extract(payload_json,'$.delegated_from_task_id')=?2",rusqlite::params![task.agent_id,task.id],|r|r.get(0))?;
            assert_eq!(children, 1, "no second physical spawn");
            runtime.join_owned_mutations().await?;
            host.shutdown().await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn actual_spawn_publishes_original_handle_before_host_kernel_can_start() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    let at = runtime.now();
    let id = crate::ids::task_id();
    let task = TaskRecord {
        id: id.clone(),
        agent_id: runtime.instance_agent_id().into(),
        kind: TaskKind::SubagentTask,
        status: TaskStatus::Queued,
        created_at: at,
        updated_at: at,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original handle publication".into()),
        detail: Some(
            serde_json::json!({"workspace_mode":"inherit","rejoin_obligation_id":id,"rejoin_generation":1}),
        ),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "original handle publication".into(),
            prompt: "original finite spawn".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    runtime
        .persist_task_transition(&task, "handle_publication_fixture")
        .await?;
    let handles = runtime.inner.task_handles.lock().await;
    let spawning = runtime.clone();
    let incoming = task.clone();
    let caller = tokio::spawn(async move {
        spawning
            .spawn_child_agent_task(
                incoming,
                "original finite spawn".into(),
                AuthorityClass::OperatorInstruction,
                false,
                false,
            )
            .await
    });
    let (lease, source) = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = runtime
                .inner
                .control_cancellations
                .original_child_spawn_for_test(&id)
            {
                break value;
            }
            tokio::task::yield_now().await;
        }
    })
    .await?;
    // The real caller is blocked on the held map. Registration must not precede
    // that publication, even though its exact accepted source already exists.
    assert!(!source.original_child_spawn_registered());
    let count:usize=runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM agent_identities WHERE json_extract(payload_json,'$.parent_agent_id')=?1 AND json_extract(payload_json,'$.delegated_from_task_id')=?2",rusqlite::params![task.agent_id,task.id],|r|r.get(0))?;
    assert_eq!(count, 0);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    source.set_original_spawn_identity_gate_for_test(gate.clone())?;
    let registration_return = Arc::new(tokio::sync::Semaphore::new(0));
    source.set_original_spawn_registration_return_gate_for_test(registration_return.clone())?;
    drop(handles);
    let created = tokio::time::timeout(
        Duration::from_secs(10),
        source.wait_original_spawn_identity_for_test(),
    )
    .await??;
    caller.abort();
    assert!(
        caller.await.unwrap_err().is_cancelled(),
        "caller must still be suspended after actual Host registration"
    );
    registration_return.add_permits(1024);
    {
        let handles = runtime.inner.task_handles.lock().await;
        let Some(ManagedTaskHandle::Async(handle)) = handles.get(&id) else {
            anyhow::bail!("actual child has no public original handle")
        };
        assert!(Arc::ptr_eq(&handle.0, &lease));
    }
    gate.add_permits(1);
    tokio::time::timeout(
        Duration::from_secs(15),
        runtime.continue_original_child_spawn(source.clone()),
    )
    .await??;
    assert_eq!(
        source
            .monitor_stage()
            .context("same source lost actual child")?
            .child,
        created.0
    );
    assert!(
        runtime
            .continue_original_child_monitor_binding(source.clone())
            .await?
    );
    let stopped = runtime
        .stop_task(&id, &AuthorityClass::OperatorInstruction)
        .await?;
    assert_eq!(stopped.status, TaskStatus::Cancelled);
    runtime
        .continue_original_child_terminal(source.clone())
        .await?;
    assert!(source.is_durable_finished());
    runtime.join_owned_mutations().await?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cancelled_before_handle_publication_hands_unstarted_task_to_original_bootstrap(
) -> Result<()> {
    for legacy in [None, Some(false), Some(true)] {
        let worktree = legacy == Some(true);
        use crate::runtime_db::{
            child_task_terminal::observe_original_child_spawn_no_start_handoff_tx,
            task_snapshot::{TaskDomainLoadRequest, TaskSnapshotError},
        };
        let (_home, host, runtime) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        let at = runtime.now();
        let id = crate::ids::task_id();
        let recovery = match legacy {
            None => TaskRecoverySpec::ChildAgentTask {
                summary: "unstarted original monitor".into(),
                prompt: "original unstarted child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
                workspace_mode: ChildAgentWorkspaceMode::Inherit,
                lifecycle_disposition: AgentLifecycleDisposition::Retain,
            },
            Some(false) => TaskRecoverySpec::SubagentTask {
                summary: "unstarted original monitor".into(),
                prompt: "original unstarted child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            },
            Some(true) => TaskRecoverySpec::WorktreeSubagentTask {
                summary: "unstarted original monitor".into(),
                prompt: "original unstarted child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            },
        };
        let task = TaskRecord {
            id: id.clone(),
            agent_id: runtime.instance_agent_id().into(),
            kind: if worktree {
                TaskKind::WorktreeSubagentTask
            } else {
                TaskKind::SubagentTask
            },
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("unstarted original monitor".into()),
            detail: Some(
                serde_json::json!({"workspace_mode":if worktree{"worktree"}else{"inherit"},"rejoin_obligation_id":id,"rejoin_generation":1}),
            ),
            recovery: Some(recovery),
        };
        runtime
            .persist_task_transition(&task, "no_start_original_task")
            .await?;
        let original = runtime
            .runtime_db()
            .tasks()
            .latest(&id)?
            .context("actual original Task ACK missing")?;
        let handles = runtime.inner.task_handles.lock().await;
        let spawn = runtime.clone();
        let incoming = task.clone();
        let caller = tokio::spawn(async move {
            spawn
                .spawn_child_agent_task(
                    incoming,
                    "original unstarted child".into(),
                    AuthorityClass::OperatorInstruction,
                    worktree,
                    false,
                )
                .await
        });
        let (lease, source) = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(value) = runtime
                    .inner
                    .control_cancellations
                    .original_child_spawn_for_test(&id)
                {
                    break value;
                }
                tokio::task::yield_now().await;
            }
        })
        .await?;
        let completion = Arc::downgrade(&source.completion_material()?.guard());
        assert!(!source.original_child_spawn_registered());
        caller.abort();
        assert!(caller
            .await
            .expect_err("caller must be cancelled at held publication map")
            .is_cancelled());
        drop(handles);
        lease.cancel();
        lease.join().await?;
        assert!(lease.is_finished() && source.has_terminal_responsibility());
        assert!(
            !source.original_child_spawn_registered()
                && source.monitor_stage().is_none()
                && source.observed_terminal().is_none()
        );
        let token = source
            .seal_original_child_spawn_without_start()
            .await?
            .context("actual cancelled unstarted lease must seal")?;
        // The winning seal and the real Host claim share one registration mutex.
        let denied = runtime
            .inner
            .host_bridge
            .as_ref()
            .unwrap()
            .start_original_child_spawn(runtime.clone(), source.clone())
            .await
            .expect_err("sealed no-start cannot register a physical worker");
        assert!(source.original_child_spawn_cancellation_error(&denied));
        let request = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: 65536,
            byte_limit: 64 * 1024 * 1024,
        };
        let small = TaskDomainLoadRequest {
            recovery_at: None,
            query: Default::default(),
            capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
            record_limit: 128,
            byte_limit: 64 * 1024 * 1024,
        };
        let resource_refresh = runtime.hold_resource_refresh_for_test().await?;
        let baseline = runtime.inner.input_evidence_budget.report()?.owned;
        let error = runtime
            .runtime_db()
            .transaction(|tx| {
                observe_original_child_spawn_no_start_handoff_tx(
                    tx,
                    &source,
                    &token,
                    &small,
                    &runtime.inner.input_evidence_budget,
                )
            })
            .err()
            .context("native material must share finite original phase allowance")?;
        assert!(matches!(
            error.downcast_ref::<TaskSnapshotError>(),
            Some(TaskSnapshotError::ResourceExhausted(_))
        ));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.owned,
            baseline
        );
        drop(resource_refresh);
        assert!(source.has_terminal_responsibility() && completion.upgrade().is_some());
        runtime.runtime_db().transaction(|tx| {
        // Same-TX adversarial observations: neither an absent original State nor
        // a retired incarnation may decode the poisoned Task/successor State.
        tx.execute_batch("SAVEPOINT no_start_absent")?;
        tx.execute("DELETE FROM agent_states WHERE agent_id=?1",[&task.agent_id])?;
        tx.execute("UPDATE tasks SET payload_json='bad task body' WHERE task_id=?1",[&id])?;
        assert!(observe_original_child_spawn_no_start_handoff_tx(tx,&source,&token,&request,&runtime.inner.input_evidence_budget)?.is_none());
        tx.execute_batch("ROLLBACK TO no_start_absent; RELEASE no_start_absent; SAVEPOINT no_start_retired")?;
        tx.execute("UPDATE agent_identities SET payload_json=json_set(payload_json,'$.incarnation',2) WHERE agent_id=?1",[&task.agent_id])?;
        tx.execute("UPDATE agent_states SET payload_json='poison successor State' WHERE agent_id=?1",[&task.agent_id])?;
        tx.execute("UPDATE tasks SET payload_json='bad task body' WHERE task_id=?1",[&id])?;
        assert!(observe_original_child_spawn_no_start_handoff_tx(tx,&source,&token,&request,&runtime.inner.input_evidence_budget)?.is_none());
        tx.execute_batch("ROLLBACK TO no_start_retired; RELEASE no_start_retired")?;Ok(())
    })?;
        assert_eq!(runtime.runtime_db().tasks().latest(&id)?.unwrap(), original);
        for changed in ["prompt", "authority", "workspace", "recovery_kind"] {
            let mut forged = original.clone();
            forged.recovery = Some(match changed {
                "prompt" => TaskRecoverySpec::SubagentTask {
                    summary: "unstarted original monitor".into(),
                    prompt: "foreign command".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                },
                "authority" => TaskRecoverySpec::SubagentTask {
                    summary: "unstarted original monitor".into(),
                    prompt: "original unstarted child".into(),
                    authority_class: AuthorityClass::RuntimeInstruction,
                },
                "workspace" => {
                    if worktree {
                        TaskRecoverySpec::SubagentTask {
                            summary: "unstarted original monitor".into(),
                            prompt: "original unstarted child".into(),
                            authority_class: AuthorityClass::OperatorInstruction,
                        }
                    } else {
                        TaskRecoverySpec::WorktreeSubagentTask {
                            summary: "unstarted original monitor".into(),
                            prompt: "original unstarted child".into(),
                            authority_class: AuthorityClass::OperatorInstruction,
                        }
                    }
                }
                _ => TaskRecoverySpec::AgentInvocation {
                    summary: "unstarted original monitor".into(),
                    prompt: "original unstarted child".into(),
                    authority_class: AuthorityClass::OperatorInstruction,
                    target_agent_id: None,
                    created_new_subagent: true,
                    workspace_mode: ChildAgentWorkspaceMode::Inherit,
                    lifecycle_disposition: AgentLifecycleDisposition::Retain,
                },
            });
            assert!(
                token.validate_durable_task(&source, &forged).is_err(),
                "NoStart cannot adopt changed {changed}"
            );
        }
        // Actual original continuation consumes the sealed observation under the
        // Host original-scope owner; it does not fabricate a terminal Task or receipt.
        runtime
            .continue_original_child_terminal(source.clone())
            .await?;
        assert!(source.is_durable_finished() && !source.has_terminal_responsibility());
        assert_eq!(runtime.runtime_db().tasks().latest(&id)?.unwrap(), original);
        let children:usize=runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM agent_identities WHERE json_extract(payload_json,'$.parent_agent_id')=?1 AND json_extract(payload_json,'$.delegated_from_task_id')=?2",rusqlite::params![task.agent_id,id],|row|row.get(0))?;
        let messages:usize=runtime.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM messages WHERE json_extract(payload_json,'$.metadata.task_id')=?1",[&id],|row|row.get(0))?;
        let stops: usize = runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM task_stop_effects WHERE task_id=?1",
            [&id],
            |row| row.get(0),
        )?;
        assert_eq!((children, messages, stops), (0, 0, 0));
        drop(token);
        assert!(
            completion.upgrade().is_none(),
            "confirmed handoff must release the original command body"
        );
        runtime.join_owned_mutations().await?;
        host.unload_runtime(runtime.instance_agent_id()).await;
        let current = host.default_runtime().await?;
        current.wait_for_bootstrap().await?;
        assert!(current.instance_owner_epoch() > runtime.instance_owner_epoch());
        let interrupted = current
            .runtime_db()
            .tasks()
            .latest(&id)?
            .context("bootstrap must retain original Task")?;
        assert_eq!(interrupted.status, TaskStatus::Interrupted);
        assert_eq!(interrupted.recovery, original.recovery);
        let children:usize=current.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM agent_identities WHERE json_extract(payload_json,'$.parent_agent_id')=?1 AND json_extract(payload_json,'$.delegated_from_task_id')=?2",rusqlite::params![task.agent_id,id],|row|row.get(0))?;
        assert_eq!(
            children, 0,
            "bootstrap disposition must not start an unbound physical child"
        );
        current.join_owned_mutations().await?;
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn actual_qualified_legacy_child_recovery_keeps_accepted_command_through_unload_reload(
) -> Result<()> {
    for worktree in [false, true] {
        let (_home, host, runtime) =
            crate::runtime::tests::support::host_backed_test_runtime().await;
        if worktree {
            let repo = runtime.workspace_root();
            std::fs::create_dir_all(&repo)?;
            for args in [
                vec!["init", "--quiet"],
                vec!["config", "user.email", "legacy-spawn@localhost"],
                vec!["config", "user.name", "Legacy Spawn"],
            ] {
                let output = std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .output()?;
                ensure!(
                    output.status.success(),
                    "legacy worktree Git setup: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::fs::write(repo.join("tracked"), "original\n")?;
            for args in [
                vec!["add", "tracked"],
                vec!["commit", "--quiet", "-m", "original"],
            ] {
                let output = std::process::Command::new("git")
                    .args(args)
                    .current_dir(&repo)
                    .output()?;
                ensure!(
                    output.status.success(),
                    "legacy worktree Git setup: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
        }
        let at = runtime.now();
        let id = crate::ids::task_id();
        let recovery = if worktree {
            TaskRecoverySpec::WorktreeSubagentTask {
                summary: "existing legacy recovery".into(),
                prompt: "actual legacy child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            }
        } else {
            TaskRecoverySpec::SubagentTask {
                summary: "existing legacy recovery".into(),
                prompt: "actual legacy child".into(),
                authority_class: AuthorityClass::OperatorInstruction,
            }
        };
        let task = TaskRecord {
            id: id.clone(),
            agent_id: runtime.instance_agent_id().into(),
            kind: if worktree {
                TaskKind::WorktreeSubagentTask
            } else {
                TaskKind::SubagentTask
            },
            status: TaskStatus::Queued,
            created_at: at,
            updated_at: at,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("existing legacy recovery".into()),
            detail: Some(
                serde_json::json!({"workspace_mode":if worktree{"worktree"}else{"inherit"},"rejoin_obligation_id":id,"rejoin_generation":1}),
            ),
            recovery: Some(recovery),
        };
        runtime
            .persist_task_transition(&task, "actual_legacy_child_task")
            .await?;
        let (registration, lease, source) = runtime.admit_original_child_monitor(&task).await?;
        let model = runtime.model_state_for(&registration.state);
        let model = AgentModelResolution {
            requested: None,
            resolved_provider: model.effective_model.provider.as_str().into(),
            resolved_model: model.effective_model.model,
            resolved_parameters: None,
            resolution_status: AgentModelResolutionStatus::Inherited,
            policy_notes: vec![],
        };
        for mismatch in ["prompt", "authority", "workspace", "lifecycle"] {
            let result = source.retain_original_child_spawn(
                task.clone(),
                if mismatch == "prompt" {
                    "foreign prompt"
                } else {
                    "actual legacy child"
                }
                .into(),
                if mismatch == "authority" {
                    AuthorityClass::RuntimeInstruction
                } else {
                    AuthorityClass::OperatorInstruction
                },
                if mismatch == "workspace" {
                    !worktree
                } else {
                    worktree
                },
                false,
                if mismatch == "lifecycle" {
                    AgentLifecycleDisposition::Retain
                } else {
                    AgentLifecycleDisposition::DeleteOnTerminal
                },
                model.clone(),
                at,
            );
            assert!(
                result.is_err(),
                "legacy accepted command must reject changed {mismatch}"
            );
            assert!(!source.original_child_spawn_pending());
        }
        source.retain_original_child_spawn(
            task.clone(),
            "actual legacy child".into(),
            AuthorityClass::OperatorInstruction,
            worktree,
            false,
            AgentLifecycleDisposition::DeleteOnTerminal,
            model,
            at,
        )?;
        let identity_gate = Arc::new(tokio::sync::Semaphore::new(0));
        source.set_original_spawn_identity_gate_for_test(identity_gate.clone())?;
        // This is an actual original registered observer. Only its wait is
        // cancelled; the Host owns the finite accepted physical kernel.
        let (_send, wait) = tokio::sync::oneshot::channel::<()>();
        lease.attach(tokio::spawn(async move {
            let _ = wait.await;
        }));
        runtime.inner.task_handles.lock().await.insert(
            id.clone(),
            ManagedTaskHandle::Async(SharedControlTaskHandle(lease.clone())),
        );
        drop(registration);
        runtime
            .inner
            .host_bridge
            .as_ref()
            .unwrap()
            .start_original_child_spawn(runtime.clone(), source.clone())
            .await?;
        let child = tokio::time::timeout(
            Duration::from_secs(10),
            source.wait_original_spawn_identity_for_test(),
        )
        .await??;
        let dispatch = Arc::new(tokio::sync::Semaphore::new(0));
        host.set_clock_recovery_dispatch_gate_for_test(&child.0, dispatch.clone());
        identity_gate.add_permits(1);
        tokio::time::timeout(
            Duration::from_secs(15),
            runtime.continue_original_child_spawn(source.clone()),
        )
        .await??;
        let stage = source
            .monitor_stage()
            .context("actual legacy Host kernel must retain its monitor stage")?;
        assert_eq!(stage.worktree, worktree);
        assert_eq!(stage.lifecycle, AgentLifecycleDisposition::DeleteOnTerminal);
        assert!(
            runtime
                .continue_original_child_monitor_binding(source.clone())
                .await?
        );
        let bound = runtime
            .runtime_db()
            .tasks()
            .latest(&id)?
            .context("legacy binding Task missing")?;
        assert_eq!(
            bound.recovery, task.recovery,
            "typed binding must preserve legacy recovery bytes"
        );
        lease.cancel();
        lease.join().await?;
        runtime
            .continue_original_child_terminal(source.clone())
            .await?;
        assert!(
            source.is_durable_finished(),
            "complete original bound legacy Task must own normal recovery"
        );
        host.unload_runtime(runtime.instance_agent_id()).await;
        let current = host.default_runtime().await?;
        assert!(current.instance_owner_epoch() > runtime.instance_owner_epoch());
        // Host bootstrap has actually called recover_supervised_child_tasks;
        // a legacy guard rejection would leave Interrupted and no real lease.
        let recovered = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let source = {
                    let handles = current.inner.task_handles.lock().await;
                    handles
                        .get(&id)
                        .and_then(ManagedTaskHandle::control_lease)
                        .map(|lease| lease.original_child())
                        .transpose()?
                        .flatten()
                };
                if let Some(source) = source {
                    break Ok::<_, anyhow::Error>(source);
                }
                if current
                    .runtime_db()
                    .tasks()
                    .latest(&id)?
                    .is_some_and(|task| task.status == TaskStatus::Interrupted)
                {
                    anyhow::bail!(
                        "qualified legacy Task was incorrectly rejected by original spawn guard"
                    )
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        assert_eq!(
            recovered.source_view()?.admitted_task().recovery,
            task.recovery
        );
        assert_eq!(
            current.runtime_db().tasks().latest(&id)?.unwrap().recovery,
            task.recovery
        );
        dispatch.add_permits(1024);
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if current
                    .runtime_db()
                    .tasks()
                    .latest(&id)?
                    .is_some_and(|task| task.status == TaskStatus::Completed)
                    && recovered.is_durable_finished()
                {
                    break Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await??;
        let terminal = current.runtime_db().tasks().latest(&id)?.unwrap();
        assert_eq!(terminal.created_at, task.created_at);
        assert_eq!(terminal.recovery, task.recovery);
        let children:usize=current.runtime_db().connection()?.query_row("SELECT COUNT(*) FROM agent_identities WHERE json_extract(payload_json,'$.parent_agent_id')=?1 AND json_extract(payload_json,'$.delegated_from_task_id')=?2",rusqlite::params![task.agent_id,id],|row|row.get(0))?;
        assert_eq!(
            children, 1,
            "legacy recovery cannot create a replacement physical child"
        );
        drop(stage);
        current.join_owned_mutations().await?;
        host.shutdown().await?;
    }
    Ok(())
}
