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

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle, WorkspaceEntry)> {
    let home = tempdir()?;
    let path = home.path().join("original-attach-root");
    std::fs::create_dir(&path)?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "workspace-attach-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let workspace = WorkspaceEntry::new(
        "original-attach-workspace",
        path,
        Some("actual workspace".into()),
    );
    Ok((home, runtime, workspace))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row("SELECT (SELECT COUNT(*) FROM audit_events),
        (SELECT COUNT(*) FROM current_read_members WHERE family='event'),
        (SELECT COUNT(*) FROM current_event_native_births),(SELECT COUNT(*) FROM input_mutation_receipts)",[],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?)
}
fn commit(
    runtime: &RuntimeHandle,
    p: &PreparedInputTransition,
) -> Result<InputMutationExecution<TransitionCommit>> {
    let work = WorkDomainLoadRequest {
        capture: crate::runtime::resident_work::WorkCaptureRequest::current(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    let tasks = TaskDomainLoadRequest {
        capture: crate::runtime::resident_task::TaskCaptureRequest::current(),
        query: Default::default(),
        record_limit: 65536,
        byte_limit: 64 * 1024 * 1024,
    };
    runtime
        .runtime_db()
        .transitions_in_scope(&p.descriptor().scopes()[0])
        .commit_prepared_transition_with_work_request_and_memory_budget(
            p,
            &work,
            &runtime.inner.input_evidence_budget,
            &tasks,
        )
}
async fn frozen(
    runtime: &RuntimeHandle,
    workspace: &WorkspaceEntry,
) -> Result<OriginalWorkspaceObservationAudit> {
    let material = runtime.prepare_original_workspace_request_material(workspace)?;
    let mut owner = runtime.begin_mutation().await?;
    owner
        .state
        .attached_workspaces
        .push(workspace.workspace_id.clone());
    runtime.inner.storage.append_workspace_entry(workspace)?;
    owner
        .persist_state_with_original_workspace_request(material.clone())
        .await?;
    runtime.prepare_original_workspace_attached_observation(material, workspace)
}
#[tokio::test]
async fn actual_owned_workspace_attach_keeps_original_two_ack_boundary_and_repeat_state_noop(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let before = counts(&runtime)?;
    runtime.attach_workspace(&workspace).await?;
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
    );
    let first = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(first
        .state
        .attached_workspaces
        .contains(&workspace.workspace_id));
    assert_eq!(
        runtime
            .inner
            .storage
            .latest_workspace_entries()?
            .iter()
            .filter(|r| r.workspace_id == workspace.workspace_id)
            .count(),
        1
    );
    runtime.attach_workspace(&workspace).await?;
    assert_eq!(runtime.agent_state().await?, first.state.as_ref().clone());
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 3)
    );
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let capture = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let rows = capture
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind == "workspace_attached" => Some(e),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert_eq!(
            row.data,
            serde_json::json!({
        "workspace_id":workspace.workspace_id,"workspace_anchor":workspace.workspace_anchor})
        );
    }
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE audit_events RENAME TO forbidden_workspace_events;
        ALTER TABLE agent_states RENAME TO forbidden_workspace_states;",
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
async fn workspace_material_is_host_scoped_but_exact_owner_bound_and_rejects_before_borrowed_clone(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let loan = runtime.prepare_original_workspace_request_material(&workspace)?;
    let scope = super::super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.instance_agent_id().into(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let ledger = &runtime.inner.input_evidence_budget;
    loan.validate_binding(&scope, runtime.instance_owner_epoch(), ledger)?;
    let estimate = loan.material_estimate()?;
    let mut visits = 0;
    loan.visit_actual_guards(|g| {
        visits += 1;
        assert!(g.matches_resource_ledger(ledger));
        assert!(
            g.validate_owned_material_binding(&scope, ledger, estimate.0 as u64)
                .is_err(),
            "Host guard must not enter the Input agent-scoped guard Vec"
        );
        Ok(())
    })?;
    assert_eq!(visits, 1);
    let mut foreign = scope.clone();
    foreign.agent_id.push_str("-foreign");
    assert!(loan
        .validate_binding(&foreign, runtime.instance_owner_epoch(), ledger)
        .is_err());
    assert!(loan
        .validate_binding(&scope, runtime.instance_owner_epoch() + 1, ledger)
        .is_err());
    let other = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )?;
    assert!(loan
        .validate_binding(&scope, runtime.instance_owner_epoch(), &other)
        .is_err());
    let mut huge = workspace.clone();
    huge.repo_name = Some("x".repeat(17 * 1024 * 1024));
    let baseline = counts(&runtime)?;
    let budget = ledger.report()?.observed_bytes;
    let error = runtime
        .attach_workspace(&huge)
        .await
        .err()
        .context("oversized borrowed Workspace copied")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(counts(&runtime)?, baseline);
    assert_eq!(ledger.report()?.observed_bytes, budget);
    Ok(())
}
#[tokio::test]
async fn actual_workspace_audit_group_preserves_state_ack_on_rollback_and_recovers_original_known_or_lost_ack(
) -> Result<()> {
    for mode in ["rollback", "known", "unknown"] {
        let (_home, runtime, workspace) = fixture().await?;
        let OriginalWorkspaceObservationAudit { event, source } =
            frozen(&runtime, &workspace).await?;
        let original = (event.id.clone(), event.created_at);
        let weak = Arc::downgrade(source.workspace_request.as_ref().unwrap());
        let before = counts(&runtime)?;
        let mut owner = runtime.begin_mutation().await?;
        let state = owner.state.clone();
        let root = owner.capture_published_root()?;
        let p = PreparedInputTransition::runtime_original_workspace_audit(
            owner.input_evidence_scope(),
            event,
            root.state.clone(),
            source,
            match mode {
                "rollback" => Some(TransitionFaultPoint::AfterAuditWrites),
                "known" => Some(TransitionFaultPoint::BeforeCacheUpdate),
                _ => None,
            },
        )?;
        drop(root);
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
                            "Workspace original Audit ACK lost after actual COMMIT"
                        ))
                    },
                )
                .await
                .err()
                .context("Workspace ACK not lost")?;
            assert!(format!("{e:#}").contains("lost after actual COMMIT"));
            drop(owner);
            let r = runtime.inner.mutation_owner.reserve().await?;
            assert!(r.recovery()?.unwrap().known.is_none());
            drop(r);
            drop(runtime.begin_mutation().await?);
            let settled = counts(&runtime)?;
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE audit_events RENAME TO forbidden_workspace_replay_event;
                ALTER TABLE agent_states RENAME TO forbidden_workspace_replay_state;",
            )?;
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Replayed { .. }
            ));
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE forbidden_workspace_replay_event RENAME TO audit_events;
                ALTER TABLE forbidden_workspace_replay_state RENAME TO agent_states;",
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
            .any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.id==original.0 && e.created_at==original.1)));
    }
    Ok(())
}
#[tokio::test]
async fn accepted_workspace_attach_completes_after_caller_abort_and_closing_without_new_admission(
) -> Result<()> {
    let (_home, runtime, workspace) = fixture().await?;
    let owner = runtime.begin_mutation().await?;
    let n = runtime.inner.mutation_tasks.lock().unwrap().handles.len();
    let before = counts(&runtime)?;
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        let workspace = workspace.clone();
        async move { runtime.attach_workspace(&workspace).await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.inner.mutation_tasks.lock().unwrap().handles.len() <= n {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("Workspace request was not registered")?;
    runtime.close_mutation_admission();
    assert!(runtime.attach_workspace(&workspace).await.is_err());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    assert!(runtime
        .agent_state()
        .await?
        .attached_workspaces
        .contains(&workspace.workspace_id));
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
    );
    Ok(())
}
