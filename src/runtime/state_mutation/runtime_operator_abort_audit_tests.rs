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

async fn fixture(with_run: bool) -> Result<(tempfile::TempDir, RuntimeHandle, CancellationToken)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-operator-abort-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let token = CancellationToken::new();
    if with_run {
        let mut owner = runtime.begin_mutation().await?;
        owner.state.status = AgentStatus::AwakeRunning;
        owner.state.current_run_id = Some("original-operator-run".into());
        owner.current_run_abort = Some(CurrentRunAbortHandle {
            run_id: "original-operator-run".into(),
            token: token.clone(),
            reason: Arc::new(StdMutex::new("not-yet-aborted".into())),
        });
        owner.persist_state(&runtime.inner.storage).await?;
    }
    Ok((home, runtime, token))
}
fn counts(runtime: &RuntimeHandle) -> Result<(usize, usize, usize, usize)> {
    Ok(runtime.runtime_db().connection()?.query_row(
        "SELECT (SELECT COUNT(*) FROM audit_events),
        (SELECT COUNT(*) FROM current_read_members WHERE family='event'),
        (SELECT COUNT(*) FROM current_event_native_births),
        (SELECT COUNT(*) FROM input_mutation_receipts)",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?)
}
fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(64 * 1024 * 1024, 65536)
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
        capture: crate::runtime::resident_task::TaskCaptureRequest {
            include_active: true,
            include_unsettled: true,
            terminal_limit: 0,
            required_keys: Default::default(),
        },
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
async fn frozen(runtime: &RuntimeHandle) -> Result<OriginalOperatorAbortAudit> {
    let mut owner = runtime.begin_mutation().await?;
    scheduler::apply_stop_projection(&mut owner.state);
    owner.defer_abort_current_run("operator_aborted");
    owner.persist_state(&runtime.inner.storage).await?;
    owner.freeze_original_operator_abort_audit(CurrentRunAbortMode::StopAfterAbort)
}

#[tokio::test]
async fn actual_operator_abort_keeps_both_modes_original_state_boundary_noop_and_error_outcomes(
) -> Result<()> {
    for mode in [
        CurrentRunAbortMode::StopAfterAbort,
        CurrentRunAbortMode::IdleAfterAbort,
    ] {
        let (_home, runtime, token) = fixture(true).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let before = counts(&runtime)?;
        let before_root = runtime.inner.mutation_owner.capture()?.unwrap();
        let out = runtime
            .abort_current_run(CurrentRunAbortRequest {
                run_id: Some("original-operator-run".into()),
                mode,
            })
            .await?;
        assert_eq!(
            out,
            CurrentRunAbortOutcome {
                agent_id: runtime.instance_agent_id().into(),
                run_id: "original-operator-run".into(),
                mode,
            }
        );
        assert!(token.is_cancelled());
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state.current_run_id, None);
        assert_eq!(
            root.state.status,
            match mode {
                CurrentRunAbortMode::StopAfterAbort => AgentStatus::Stopped,
                CurrentRunAbortMode::IdleAfterAbort => AgentStatus::AwakeIdle,
            }
        );
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
        );
        assert_eq!(
            root.current_reads.as_ref().unwrap().coverage().revision,
            before_root
                .current_reads
                .as_ref()
                .unwrap()
                .coverage()
                .revision
                + 2
        );
        let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
        let abort = selected
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == "current_run_aborted" => Some(e),
                _ => None,
            })
            .context("operator abort was not qualified")?;
        assert_eq!(abort.data["agent_id"], out.agent_id);
        assert_eq!(abort.data["run_id"], out.run_id);
        assert_eq!(abort.data["mode"], mode.as_str());
        assert_eq!(abort.data["reason"], "operator_aborted");
        assert!(selected
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
            if e.kind=="agent_state_mutated" && e.event_seq+1==abort.event_seq)));
        assert_eq!(
            *runtime
                .inner
                .agent
                .lock()
                .await
                .current_run_abort
                .as_ref()
                .unwrap()
                .reason
                .lock()
                .unwrap(),
            "operator_aborted"
        );
        // The original abort handle remains usable for a real State noop. It
        // must still append its observation without inventing another State ACK.
        let repeat = runtime
            .abort_current_run(CurrentRunAbortRequest { run_id: None, mode })
            .await?;
        assert_eq!(repeat, out);
        assert_eq!(runtime.agent_state().await?, root.state.as_ref().clone());
        assert_eq!(
            counts(&runtime)?,
            (before.0 + 3, before.1 + 3, before.2 + 3, before.3 + 3)
        );
        let rejected = counts(&runtime)?;
        let error = runtime
            .abort_current_run(CurrentRunAbortRequest {
                run_id: Some("stale-original-run".into()),
                mode,
            })
            .await
            .err()
            .context("stale operator run accepted")?;
        assert!(matches!(error.downcast_ref::<CurrentRunAbortError>(),
            Some(CurrentRunAbortError::StaleRunId{requested_run_id,current_run_id})
            if requested_run_id=="stale-original-run" && current_run_id=="original-operator-run"));
        assert_eq!(counts(&runtime)?, rejected);
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE audit_events RENAME TO forbidden_operator_audit;
            ALTER TABLE agent_states RENAME TO forbidden_operator_state;",
        )?;
        assert!(root
            .current_reads
            .as_ref()
            .unwrap()
            .capture(&request())?
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==abort.id)));
    }
    let (_home, runtime, _) = fixture(false).await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = counts(&runtime)?;
    let error = runtime
        .abort_current_run(CurrentRunAbortRequest {
            run_id: None,
            mode: CurrentRunAbortMode::StopAfterAbort,
        })
        .await
        .err()
        .context("missing current run accepted")?;
    assert!(matches!(error.downcast_ref::<CurrentRunAbortError>(),
        Some(CurrentRunAbortError::NoCurrentRun{agent_id}) if agent_id==runtime.instance_agent_id()));
    assert_eq!(counts(&runtime)?, before);
    Ok(())
}

#[tokio::test]
async fn original_operator_abort_audit_rolls_back_or_recovers_known_and_unknown_without_reaborting(
) -> Result<()> {
    for mode in ["rollback", "known", "unknown"] {
        let (_home, runtime, token) = fixture(true).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalOperatorAbortAudit {
            event,
            source,
            outcome,
        } = frozen(&runtime).await?;
        assert!(token.is_cancelled());
        assert_eq!(outcome.run_id, "original-operator-run");
        drop(outcome);
        let original = (event.id.clone(), event.created_at);
        let weak = Arc::downgrade(&source._material);
        let baseline = counts(&runtime)?;
        let mut owner = runtime.begin_mutation().await?;
        let state = owner.state.clone();
        let root = owner.capture_published_root()?;
        let p = PreparedInputTransition::runtime_operator_abort_audit(
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
            let error = commit(&runtime, &p)
                .err()
                .context("operator Audit did not roll back")?;
            assert!(format!("{error:#}").contains("AfterAuditWrites"));
            assert_eq!(counts(&runtime)?, baseline);
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
            let error = owner
                .commit_transition_inner(
                    p.descriptor().command_nonce().into(),
                    Some(witness),
                    || {
                        assert!(matches!(
                            commit(&runtime, &p)?,
                            InputMutationExecution::Committed { .. }
                        ));
                        Err(anyhow!(
                            "original operator Audit ACK lost after real COMMIT"
                        ))
                    },
                )
                .await
                .err()
                .context("operator Audit ACK not lost")?;
            assert!(format!("{error:#}").contains("lost after real COMMIT"));
            drop(owner);
            let reservation = runtime.inner.mutation_owner.reserve().await?;
            assert!(reservation.recovery()?.unwrap().known.is_none());
            drop(reservation);
            drop(runtime.begin_mutation().await?);
            let settled = counts(&runtime)?;
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO forbidden_abort_replay_state;
                ALTER TABLE audit_events RENAME TO forbidden_abort_replay_audit;",
            )?;
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Replayed { .. }
            ));
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE forbidden_abort_replay_state RENAME TO agent_states;
                ALTER TABLE forbidden_abort_replay_audit RENAME TO audit_events;",
            )?;
            assert_eq!(counts(&runtime)?, settled);
            drop(p);
            assert!(weak.upgrade().is_none());
        }
        assert_eq!(
            counts(&runtime)?,
            (
                baseline.0 + 1,
                baseline.1 + 1,
                baseline.2 + 1,
                baseline.3 + 1
            )
        );
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state.as_ref(), &state);
        assert!(root.current_reads.as_ref().unwrap().capture(&request())?.events.iter()
            .any(|r|matches!(&r.fact,CurrentReadFact::Event(e) if e.id==original.0 && e.created_at==original.1)));
    }
    Ok(())
}

#[tokio::test]
async fn original_operator_abort_preserves_deleting_state_some_and_rejects_no_state_successor_or_other_family(
) -> Result<()> {
    for mode in ["deleting", "no_state", "retired", "model_factory"] {
        let (_home, runtime, _) = fixture(true).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalOperatorAbortAudit {
            event,
            source,
            outcome,
        } = frozen(&runtime).await?;
        drop(outcome);
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let baseline = counts(&runtime)?;
        let scope = source.scope.clone();
        if mode == "model_factory" {
            let weak = Arc::downgrade(&source._material);
            let error = PreparedInputTransition::runtime_lifecycle_noop_audit(
                scope,
                event,
                root.state.clone(),
                source,
                None,
            )
            .err()
            .context("Model factory adopted operator source")?;
            assert!(format!("{error:#}").contains("another original Lifecycle family"));
            assert!(weak.upgrade().is_none());
            assert_eq!(counts(&runtime)?, baseline);
            continue;
        }
        let p = PreparedInputTransition::runtime_operator_abort_audit(
            scope,
            event,
            root.state.clone(),
            source,
            None,
        )?;
        let mut identity = runtime
            .runtime_db()
            .agent_identities()
            .latest(runtime.instance_agent_id())?
            .unwrap();
        let original_identity = identity.clone();
        match mode {
            "deleting" => {
                identity.status = crate::types::AgentRegistryStatus::Deleting;
                runtime.runtime_db().agent_identities().upsert(&identity)?;
            }
            "no_state" => {
                runtime.runtime_db().connection()?.execute(
                    "DELETE FROM agent_states WHERE agent_id=?1",
                    [runtime.instance_agent_id()],
                )?;
            }
            _ => {
                identity.incarnation += 1;
                runtime.runtime_db().agent_identities().upsert(&identity)?;
                runtime.runtime_db().connection()?.execute("UPDATE agent_states SET payload_json='poison-abort-successor' WHERE agent_id=?1",[runtime.instance_agent_id()])?;
            }
        }
        if mode == "deleting" {
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Committed { .. }
            ));
            assert_eq!(
                counts(&runtime)?,
                (
                    baseline.0 + 1,
                    baseline.1 + 1,
                    baseline.2 + 1,
                    baseline.3 + 1
                )
            );
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(root.state.as_ref())
            );
        } else {
            let charged = runtime.inner.input_evidence_budget.report()?.observed_bytes;
            let error = commit(&runtime, &p)
                .err()
                .with_context(|| format!("operator {mode} gained authority"))?;
            assert!(
                matches!(
                    error.downcast_ref::<CurrentReadError>(),
                    Some(CurrentReadError::RetiredScope)
                ),
                "{mode}: {error:#}"
            );
            assert_eq!(counts(&runtime)?, baseline);
            assert_eq!(
                runtime.inner.input_evidence_budget.report()?.observed_bytes,
                charged
            );
        }
        runtime
            .runtime_db()
            .agent_identities()
            .upsert(&original_identity)?;
        runtime.runtime_db().transaction(|tx| {
            crate::runtime_db::evidence::upsert_agent_state_tx(tx, &root.state)
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn admitted_operator_abort_survives_caller_cancellation_and_closing_while_new_request_rejects(
) -> Result<()> {
    let (_home, runtime, token) = fixture(true).await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let owner = runtime.begin_mutation().await?;
    let before = counts(&runtime)?;
    let n = runtime.inner.mutation_tasks.lock().unwrap().handles.len();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        async move {
            runtime
                .abort_current_run(CurrentRunAbortRequest {
                    run_id: Some("original-operator-run".into()),
                    mode: CurrentRunAbortMode::StopAfterAbort,
                })
                .await
        }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.inner.mutation_tasks.lock().unwrap().handles.len() <= n {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("operator request was not actually registered")?;
    runtime.close_mutation_admission();
    assert!(runtime
        .abort_current_run(CurrentRunAbortRequest {
            run_id: None,
            mode: CurrentRunAbortMode::StopAfterAbort
        })
        .await
        .is_err());
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    assert!(token.is_cancelled());
    assert_eq!(runtime.agent_state().await?.status, AgentStatus::Stopped);
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 2, before.1 + 2, before.2 + 2, before.3 + 2)
    );
    Ok(())
}
