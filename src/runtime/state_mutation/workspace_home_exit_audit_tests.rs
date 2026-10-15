use super::*;
use crate::runtime::{
    current_read_facts::{CurrentReadError, CurrentReadFact, CurrentReadRequest},
    tests::support::{context_config, tempdir},
};
use crate::runtime_db::{
    task_snapshot::TaskDomainLoadRequest,
    transitions::{InputMutationExecution, PreparedInputTransition},
    work_snapshot::WorkDomainLoadRequest,
};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, ActiveWorkspaceEntry)> {
    let home = tempdir()?;
    let external = home.path().join("original-external-root");
    std::fs::create_dir(&external)?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "workspace-home-exit-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let workspace = WorkspaceEntry::new("original-external-workspace", external.clone(), None);
    runtime.inner.storage.append_workspace_entry(&workspace)?;
    let entry = ActiveWorkspaceEntry {
        workspace_id: workspace.workspace_id.clone(),
        workspace_anchor: workspace.workspace_anchor.clone(),
        execution_root_id: "original-external-execution-root".into(),
        execution_root: external.clone(),
        projection_kind: WorkspaceProjectionKind::CanonicalRoot,
        access_mode: WorkspaceAccessMode::ExclusiveWrite,
        cwd: external,
        occupancy_id: None,
        projection_metadata: None,
    };
    let mut owner = runtime.begin_mutation().await?;
    owner.state.attached_workspaces.push(workspace.workspace_id);
    owner.state.active_workspace_entry = Some(entry.clone());
    owner.state.execution_profile.allow_background_tasks = false;
    owner.persist_state(&runtime.inner.storage).await?;
    drop(owner);
    Ok((home, runtime, entry))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),
        (SELECT COUNT(*) FROM current_read_members WHERE family='event'),
        (SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)", [],
        |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
fn commit(
    runtime: &RuntimeHandle,
    p: &PreparedInputTransition,
) -> Result<InputMutationExecution<TransitionCommit>> {
    runtime
        .runtime_db()
        .transitions_in_scope(&p.descriptor().scopes()[0])
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &WorkDomainLoadRequest {
                capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
                record_limit: 65536,
                byte_limit: 64 * 1024 * 1024,
            },
            &runtime.inner.input_evidence_budget,
            &TaskDomainLoadRequest {
                capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
                query: Default::default(),
                record_limit: 65536,
                byte_limit: 64 * 1024 * 1024,
            },
        )
}
#[tokio::test]
async fn actual_workspace_exit_keeps_original_home_used_then_exited_order_and_old_boundary(
) -> Result<()> {
    let (_home, runtime, old) = fixture().await?;
    let profile = runtime.agent_state().await?.execution_profile;
    let before = counts(&runtime)?;
    runtime.exit_workspace().await?;
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 3)
    );
    let after = runtime.agent_state().await?;
    let active = after.active_workspace_entry.as_ref().unwrap();
    assert_eq!(
        active.workspace_id,
        crate::types::agent_home_workspace_id(&after.id)
    );
    assert_eq!(active.access_mode, WorkspaceAccessMode::SharedRead);
    assert_eq!(
        active.projection_kind,
        WorkspaceProjectionKind::CanonicalRoot
    );
    assert!(after.worktree_session.is_none());
    assert!(
        old.execution_root.exists(),
        "exit does not remove the original physical root"
    );
    let (changed, used, exited): (i64, i64, i64) = runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT MAX(event_seq) FROM audit_events WHERE kind='agent_state_mutated'),
        (SELECT MAX(event_seq) FROM audit_events WHERE kind='workspace_used'),
        (SELECT MAX(event_seq) FROM audit_events WHERE kind='workspace_exited')",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    assert!(changed < used && used < exited);
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let read = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let e = read
        .events
        .iter()
        .find_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind == "workspace_exited" => Some(e),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        e.data,
        serde_json::json!({
            "workspace_id":old.workspace_id,"execution_root_id":old.execution_root_id,
            "projection_kind":old.projection_kind,"access_mode":old.access_mode,
            "boundary":crate::system::HostLocalBoundary::from_parts(&profile,Some(old.projection_kind),Some(old.access_mode),Some(old.execution_root_id.clone())).audit_metadata()
        })
    );
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_home_exit_events;
        ALTER TABLE agent_states RENAME TO forbidden_home_exit_states;",
    )?;
    assert_eq!(
        root.current_reads
            .as_ref()
            .unwrap()
            .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?
            .events
            .len(),
        read.events.len()
    );
    Ok(())
}
#[tokio::test]
async fn actual_home_repeat_is_state_noop_and_active_detach_keeps_original_order() -> Result<()> {
    let (_home, runtime, old) = fixture().await?;
    runtime
        .activate_agent_home(WorkspaceAccessMode::SharedRead, None)
        .await?;
    let state = runtime.agent_state().await?;
    let before = counts(&runtime)?;
    runtime
        .activate_agent_home(WorkspaceAccessMode::SharedRead, None)
        .await?;
    assert_eq!(runtime.agent_state().await?, state);
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    // The original active-detach path really switches Home before its State tail.
    let mut owner = runtime.begin_mutation().await?;
    owner.state.active_workspace_entry = Some(old.clone());
    owner.persist_state(&runtime.inner.storage).await?;
    drop(owner);
    let before = counts(&runtime)?;
    runtime.detach_workspace(&old.workspace_id).await?;
    let state = runtime.agent_state().await?;
    assert!(!state.attached_workspaces.contains(&old.workspace_id));
    assert_eq!(
        state.active_workspace_entry.as_ref().unwrap().workspace_id,
        crate::types::agent_home_workspace_id(&state.id)
    );
    assert_eq!(
        counts(&runtime)?,
        // Home State + used + (detach State mutation and its original tail):
        // four Events belong to three original whole receipts.
        (before.0 + 4, before.1 + 4, before.2 + 4, before.3 + 3)
    );
    assert!(old.execution_root.exists());
    Ok(())
}
#[tokio::test]
async fn accepted_home_exit_detach_and_switch_continue_after_caller_abort_and_closing() -> Result<()>
{
    for mode in ["home", "exit", "detach", "switch"] {
        let (_home, runtime, old) = fixture().await?;
        let owner = runtime.begin_mutation().await?;
        let n = runtime.inner.mutation_tasks.lock().unwrap().handles.len();
        let before = counts(&runtime)?;
        let caller = tokio::spawn({
            let runtime = runtime.clone();
            let id = old.workspace_id.clone();
            async move {
                match mode {
                    "home" => {
                        runtime
                            .activate_agent_home(WorkspaceAccessMode::SharedRead, None)
                            .await
                    }
                    "exit" => runtime.exit_workspace().await,
                    "detach" => runtime.detach_workspace(&id).await,
                    _ => runtime
                        .switch_workspace_target(
                            crate::runtime::workspace_control::WorkspaceSwitchTarget::WorkspaceId(
                                AGENT_HOME_WORKSPACE_ID.into(),
                            ),
                            None,
                        )
                        .await
                        .map(|_| ()),
                }
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while runtime.inner.mutation_tasks.lock().unwrap().handles.len() <= n {
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("Workspace continuation was not registered")?;
        runtime.close_mutation_admission();
        assert!(runtime.exit_workspace().await.is_err());
        caller.abort();
        assert!(caller.await.unwrap_err().is_cancelled());
        drop(owner);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.join_owned_mutations(),
        )
        .await??;
        let state = runtime.agent_state().await?;
        assert_eq!(
            state.active_workspace_entry.as_ref().unwrap().workspace_id,
            crate::types::agent_home_workspace_id(&state.id),
            "mode {mode}"
        );
        let extra = if mode == "exit" || mode == "detach" {
            3
        } else {
            2
        };
        // Active detach has two Events in its final State receipt.
        let extra_events = if mode == "detach" { 4 } else { extra };
        assert_eq!(
            counts(&runtime)?,
            (
                before.0 + extra_events,
                before.1 + extra_events,
                before.2 + extra_events,
                before.3 + extra
            ),
            "mode {mode}"
        );
        assert!(old.execution_root.exists());
    }
    Ok(())
}
#[tokio::test]
async fn original_exited_audit_preserves_home_ack_on_rollback_known_and_lost_ack() -> Result<()> {
    for mode in ["rollback", "known", "unknown"] {
        let (_home, runtime, old) = fixture().await?;
        let profile = runtime.agent_state().await?.execution_profile;
        let request = runtime
            .prepare_original_workspace_request_material(&runtime.inner.storage.data_dir())?;
        runtime
            .activate_agent_home_owned(WorkspaceAccessMode::SharedRead, None, request.clone())
            .await?;
        let OriginalWorkspaceObservationAudit { event, source } =
            runtime.prepare_original_workspace_exited_observation(request, &old, &profile)?;
        let original = (event.id.clone(), event.created_at);
        let weak = Arc::downgrade(source.workspace_request.as_ref().unwrap());
        let before = counts(&runtime)?;
        let mut owner = runtime.begin_mutation().await?;
        let state = owner.state.clone();
        let p = PreparedInputTransition::runtime_original_workspace_audit(
            owner.input_evidence_scope(),
            event,
            owner.capture_published_root()?.state.clone(),
            source,
            match mode {
                "rollback" => Some(TransitionFaultPoint::AfterAuditWrites),
                "known" => Some(TransitionFaultPoint::BeforeCacheUpdate),
                _ => None,
            },
        )?;
        if mode == "rollback" {
            assert!(commit(&runtime, &p).is_err());
            assert_eq!(counts(&runtime)?, before);
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(&state)
            );
            drop((p, owner));
            assert!(weak.upgrade().is_none());
            continue;
        }
        if mode == "known" {
            assert!(owner.commit_prepared_input_transition(p).await.is_err());
            drop(owner);
            assert!(matches!(
                runtime.inner.mutation_owner.capture(),
                Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
            ));
            drop(runtime.begin_mutation().await?);
            assert!(weak.upgrade().is_none());
        } else {
            let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
                p.descriptor().command_nonce().into(),
                crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
                    p.descriptor().clone(),
                ),
                Vec::new(),
            )?
            .with_input_runtime(vec![Arc::new(owner.facts(Default::default())?)])?;
            let e = owner
                .commit_transition_inner(
                    p.descriptor().command_nonce().into(),
                    Some(witness),
                    || {
                        assert!(matches!(
                            commit(&runtime, &p)?,
                            InputMutationExecution::Committed { .. }
                        ));
                        Err(anyhow!(
                            "original Workspace exit ACK lost after actual COMMIT"
                        ))
                    },
                )
                .await
                .err()
                .context("exit ACK not lost")?;
            assert!(format!("{e:#}").contains("lost after actual COMMIT"));
            drop(owner);
            let r = runtime.inner.mutation_owner.reserve().await?;
            assert!(r.recovery()?.unwrap().known.is_none());
            drop(r);
            drop(runtime.begin_mutation().await?);
            let settled = counts(&runtime)?;
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE audit_events RENAME TO forbidden_exit_replay_events;
                ALTER TABLE agent_states RENAME TO forbidden_exit_replay_states;",
            )?;
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Replayed { .. }
            ));
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE forbidden_exit_replay_events RENAME TO audit_events;
                ALTER TABLE forbidden_exit_replay_states RENAME TO agent_states;",
            )?;
            assert_eq!(counts(&runtime)?, settled);
            drop(p);
            assert!(weak.upgrade().is_none());
        }
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
        );
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state.as_ref(), &state);
        assert!(root.current_reads.as_ref().unwrap().capture(&CurrentReadRequest::current(64*1024*1024,65536))?.events.iter()
            .any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.id==original.0&&e.created_at==original.1)));
        assert!(old.execution_root.exists());
    }
    Ok(())
}
#[tokio::test]
async fn original_exited_boundary_is_budgeted_before_duplicate_root_id_json_copies() -> Result<()> {
    let (_home, runtime, mut old) = fixture().await?;
    let profile = runtime.agent_state().await?.execution_profile;
    let request =
        runtime.prepare_original_workspace_request_material(&runtime.inner.storage.data_dir())?;
    old.execution_root_id = "x".repeat(17 * 1024 * 1024);
    let before = counts(&runtime)?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let error = runtime
        .prepare_original_workspace_exited_observation(request.clone(), &old, &profile)
        .err()
        .context("oversized exited boundary copied")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert_eq!(
        runtime
            .agent_state()
            .await?
            .active_workspace_entry
            .as_ref()
            .unwrap()
            .execution_root_id,
        "original-external-execution-root"
    );
    Ok(())
}
