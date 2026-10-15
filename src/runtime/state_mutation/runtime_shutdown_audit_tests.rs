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
use tokio_util::sync::CancellationToken;

async fn fixture(
    with_run: bool,
) -> Result<(tempfile::TempDir, RuntimeHandle, Option<CancellationToken>)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-shutdown-audit-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    let token = if with_run {
        let token = CancellationToken::new();
        let mut owner = runtime.begin_mutation().await?;
        owner.state.status = AgentStatus::AwakeRunning;
        owner.state.current_run_id = Some("actual-original-shutdown-run".into());
        owner.current_run_abort = Some(CurrentRunAbortHandle {
            run_id: "actual-original-shutdown-run".into(),
            token: token.clone(),
            reason: Arc::new(StdMutex::new("operator_aborted".into())),
        });
        owner.persist_state(&runtime.inner.storage).await?;
        Some(token)
    } else {
        None
    };
    Ok((home, runtime, token))
}
fn request() -> CurrentReadRequest {
    CurrentReadRequest::current(64 * 1024 * 1024, 65536)
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
async fn frozen(runtime: &RuntimeHandle) -> Result<OriginalRuntimeShutdownAudits> {
    runtime.retire_instance_locally("daemon_shutdown").await;
    crate::runtime::scheduler_executor::SchedulerDecisionExecutor::new(runtime)
        .request_shutdown(crate::runtime::scheduler_executor::ShutdownReason::DaemonShutdown)
        .await
}

#[tokio::test]
async fn actual_shutdown_keeps_original_state_boundary_and_qualifies_its_fixed_audit_group_after_closing(
) -> Result<()> {
    for with_run in [false, true] {
        let (_home, runtime, token) = fixture(with_run).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let baseline = counts(&runtime)?;
        runtime.request_service_shutdown().await?;
        assert!(!runtime.accepts_execution_admission());
        assert!(runtime.spawn_owned_mutation(async {}).is_err());
        if let Some(token) = &token {
            assert!(token.is_cancelled());
        }
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert!(root.state.current_run_id.is_none());
        let delta = if with_run { (3, 3, 3, 2) } else { (1, 1, 1, 1) };
        assert_eq!(
            counts(&runtime)?,
            (
                baseline.0 + delta.0,
                baseline.1 + delta.1,
                baseline.2 + delta.2,
                baseline.3 + delta.3
            )
        );
        assert_eq!(
            root.current_reads.as_ref().unwrap().coverage().revision,
            before.current_reads.as_ref().unwrap().coverage().revision
                + if with_run { 2 } else { 1 }
        );
        if !with_run {
            assert_eq!(root.state, before.state);
        }
        let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
        let primary = selected
            .events
            .iter()
            .find_map(|r| match &r.fact {
                CurrentReadFact::Event(e) if e.kind == "runtime_service_shutdown_requested" => {
                    Some(e)
                }
                _ => None,
            })
            .context("original shutdown request Audit absent")?;
        assert_eq!(primary.data["current_run_id"], serde_json::Value::Null);
        assert_eq!(
            primary.data["status"],
            serde_json::to_value(&root.state.status)?
        );
        let aborted = selected.events.iter().find_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind == "current_run_aborted" => Some(e),
            _ => None,
        });
        if with_run {
            let aborted = aborted.context("original shutdown abort Audit absent")?;
            assert_eq!(
                primary.data["aborted_run_id"],
                "actual-original-shutdown-run"
            );
            assert_eq!(aborted.data["run_id"], "actual-original-shutdown-run");
            assert_eq!(aborted.data["mode"], "shutdown");
            assert_eq!(aborted.data["reason"], "daemon_shutdown");
            assert_eq!(aborted.event_seq, primary.event_seq + 1);
            assert!(selected
                .events
                .iter()
                .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
                if e.kind=="agent_state_mutated" && e.event_seq+1==primary.event_seq)));
        } else {
            assert!(aborted.is_none());
            assert!(primary.data["aborted_run_id"].is_null());
        }
        runtime.runtime_db().connection()?.execute_batch(
            "ALTER TABLE audit_events RENAME TO forbidden_shutdown_audit;
             ALTER TABLE agent_states RENAME TO forbidden_shutdown_state;",
        )?;
        let pure = root.current_reads.as_ref().unwrap().capture(&request())?;
        assert!(pure
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e) if e.id==primary.id)));
    }
    Ok(())
}

#[tokio::test]
async fn original_shutdown_pair_rolls_back_together_and_recovers_known_or_unknown_without_new_ids(
) -> Result<()> {
    for mode in ["rollback", "known", "unknown"] {
        let (_home, runtime, token) = fixture(true).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalRuntimeShutdownAudits {
            event,
            aborted,
            source,
        } = frozen(&runtime).await?;
        assert!(token.unwrap().is_cancelled());
        let original = [
            (event.id.clone(), event.created_at),
            (
                aborted.as_ref().unwrap().id.clone(),
                aborted.as_ref().unwrap().created_at,
            ),
        ];
        let weak = Arc::downgrade(&source._material);
        let baseline = counts(&runtime)?;
        let mut owner = runtime.begin_mutation().await?;
        let original_state = owner.state.clone();
        let root = owner.capture_published_root()?;
        let p = PreparedInputTransition::runtime_shutdown_audits(
            owner.input_evidence_scope(),
            event,
            aborted,
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
                .context("shutdown pair fault failed to rollback")?;
            assert!(format!("{error:#}").contains("AfterAuditWrites"));
            assert_eq!(counts(&runtime)?, baseline);
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(&original_state)
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
                        let actual = commit(&runtime, &p)?;
                        assert!(matches!(actual, InputMutationExecution::Committed { .. }));
                        Err(anyhow!("original shutdown pair ACK lost after real COMMIT"))
                    },
                )
                .await
                .err()
                .context("shutdown pair ACK not lost")?;
            assert!(format!("{error:#}").contains("lost after real COMMIT"));
            drop(owner);
            let reservation = runtime.inner.mutation_owner.reserve().await?;
            assert!(reservation.recovery()?.unwrap().known.is_none());
            drop(reservation);
            drop(runtime.begin_mutation().await?);
            let settled = counts(&runtime)?;
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO forbidden_shutdown_replay_state;
                 ALTER TABLE audit_events RENAME TO forbidden_shutdown_replay_audit;",
            )?;
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Replayed { .. }
            ));
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE forbidden_shutdown_replay_state RENAME TO agent_states;
                 ALTER TABLE forbidden_shutdown_replay_audit RENAME TO audit_events;",
            )?;
            assert_eq!(counts(&runtime)?, settled);
            drop(p);
            assert!(weak.upgrade().is_none());
        }
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state.as_ref(), &original_state);
        assert_eq!(
            counts(&runtime)?,
            (
                baseline.0 + 2,
                baseline.1 + 2,
                baseline.2 + 2,
                baseline.3 + 1
            )
        );
        let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
        for (id, at) in original {
            assert!(selected
                .events
                .iter()
                .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
                if e.id==id && e.created_at==at)));
        }
    }
    Ok(())
}

#[tokio::test]
async fn shutdown_observation_preserves_only_original_state_some_deleting_authority_and_rejects_missing_or_successor(
) -> Result<()> {
    for mode in ["deleting", "no_state", "retired"] {
        let (_home, runtime, token) = fixture(true).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalRuntimeShutdownAudits {
            event,
            aborted,
            source,
        } = frozen(&runtime).await?;
        assert!(
            token.unwrap().is_cancelled(),
            "local retirement precedes all later authority checks"
        );
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let scope = source.scope.clone();
        let p = PreparedInputTransition::runtime_shutdown_audits(
            scope,
            event,
            aborted,
            root.state.clone(),
            source,
            None,
        )?;
        let mut identity = runtime
            .runtime_db()
            .agent_identities()
            .latest(runtime.instance_agent_id())?
            .unwrap();
        let old_identity = identity.clone();
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
                runtime.runtime_db().connection()?.execute(
                    "UPDATE agent_states SET payload_json='poison-shutdown-successor' WHERE agent_id=?1",
                    [runtime.instance_agent_id()])?;
            }
        }
        let baseline = counts(&runtime)?;
        if mode == "deleting" {
            let InputMutationExecution::Committed { value, .. } = commit(&runtime, &p)? else {
                panic!("first original shutdown pair replayed")
            };
            assert!(value.applied);
            drop(value);
            assert_eq!(
                counts(&runtime)?,
                (
                    baseline.0 + 2,
                    baseline.1 + 2,
                    baseline.2 + 2,
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
                .with_context(|| format!("shutdown {mode} gained generic authority"))?;
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
            .upsert(&old_identity)?;
        runtime.runtime_db().transaction(|tx| {
            crate::runtime_db::evidence::upsert_agent_state_tx(tx, &root.state)
        })?;
    }
    Ok(())
}

#[tokio::test]
async fn original_shutdown_source_rejects_a_missing_or_changed_companion_and_other_family(
) -> Result<()> {
    for mode in ["missing", "changed", "model_factory"] {
        let (_home, runtime, _) = fixture(true).await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalRuntimeShutdownAudits {
            event,
            mut aborted,
            source,
        } = frozen(&runtime).await?;
        let weak = Arc::downgrade(&source._material);
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let baseline = counts(&runtime)?;
        let error = match mode {
            "model_factory" => PreparedInputTransition::runtime_lifecycle_noop_audit(
                source.scope.clone(),
                event,
                root.state.clone(),
                source,
                None,
            )
            .err()
            .context("Model factory reused the shutdown source")?,
            _ => {
                if mode == "missing" {
                    aborted = None;
                } else {
                    aborted.as_mut().unwrap().data["run_id"] = serde_json::json!("another-run");
                }
                PreparedInputTransition::runtime_shutdown_audits(
                    source.scope.clone(),
                    event,
                    aborted,
                    root.state.clone(),
                    source,
                    None,
                )
                .err()
                .context("shutdown source accepted a different Audit group")?
            }
        };
        let expected = if mode == "model_factory" {
            "another original Lifecycle family"
        } else {
            "shutdown"
        };
        assert!(format!("{error:#}").contains(expected), "{mode}: {error:#}");
        assert_eq!(counts(&runtime)?, baseline);
        assert!(weak.upgrade().is_none());
    }
    Ok(())
}
