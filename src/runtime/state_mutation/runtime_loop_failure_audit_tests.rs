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
use crate::runtime_error::{describe_runtime_error, RuntimeError, RuntimeErrorDomain};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "original-runtime-loop-failure",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    {
        let mut owner = runtime.begin_mutation().await?;
        owner.state.status = AgentStatus::AwakeRunning;
        owner.state.current_run_id = Some("original-failed-runtime-run".into());
        owner.persist_state(&runtime.inner.storage).await?;
    }
    Ok((home, runtime))
}
fn original_error() -> anyhow::Error {
    RuntimeError::new(
        RuntimeErrorDomain::Task,
        "original_runtime_failure",
        "original loop failed",
    )
    .with_retryable(true)
    .with_recovery_hint("retry original runtime")
    .with_safe_context("task_id", "original-failure-task")
    .into()
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
fn frozen(runtime: &RuntimeHandle) -> Result<OriginalRuntimeLoopFailureAudit> {
    let descriptor = describe_runtime_error(&original_error());
    runtime.prepare_original_runtime_loop_failure_audit(
        runtime.instance_agent_id(),
        &descriptor.operator_message,
        &descriptor,
        0,
    )
}

#[tokio::test]
async fn actual_runtime_loop_failure_qualifies_original_audit_before_independent_state_cleanup(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    let baseline = counts(&runtime)?;
    let error = original_error();
    let descriptor = describe_runtime_error(&error);
    runtime.record_runtime_loop_failure(&error).await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(root.state.current_run_id.is_none());
    assert_eq!(
        root.state.last_runtime_failure.as_ref().unwrap().summary,
        descriptor.operator_message
    );
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest(runtime.instance_agent_id())?
            .as_ref(),
        Some(root.state.as_ref())
    );
    let selected = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let failure = selected
        .events
        .iter()
        .find_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind == "agent_runtime_loop_failed" => Some(e),
            _ => None,
        })
        .context("original runtime failure Audit absent from its published root")?;
    assert_eq!(
        failure.data,
        serde_json::json!({
            "agent_id":runtime.instance_agent_id(), "error":descriptor.operator_message,
            "domain":descriptor.domain, "code":descriptor.code, "retryable":descriptor.retryable,
            "recovery_hint":descriptor.recovery_hint, "safe_context":descriptor.safe_context,
            "source_chain":descriptor.source_chain, "recovery":"bounded_restart", "released_claims":0,
        })
    );
    assert!(selected
        .events
        .iter()
        .any(|r| matches!(&r.fact, CurrentReadFact::Event(e)
        if e.kind == "agent_state_mutated" && e.event_seq == failure.event_seq + 1)));
    assert_eq!(
        counts(&runtime)?,
        (
            baseline.0 + 2,
            baseline.1 + 2,
            baseline.2 + 2,
            baseline.3 + 2
        )
    );
    assert_eq!(
        root.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 2
    );
    let mut expected = before.state.as_ref().clone();
    expected.current_run_id = None;
    expected.last_runtime_failure = root.state.last_runtime_failure.clone();
    assert_eq!(root.state.as_ref(), &expected);
    assert_eq!(runtime.runtime_db().connection()?.query_row(
        "SELECT COUNT(*) FROM audit_events WHERE kind='queue_claim_released_for_runtime_restart'", [], |r| r.get::<_,usize>(0))?, 0);
    Ok(())
}

#[tokio::test]
async fn runtime_loop_failure_audit_rollback_does_not_skip_original_state_failure_write(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    runtime.runtime_db().connection()?.execute_batch(
        "CREATE TRIGGER reject_original_loop_failure BEFORE INSERT ON audit_events
         WHEN NEW.kind='agent_runtime_loop_failed'
         BEGIN SELECT RAISE(ABORT,'original runtime failure Audit rollback'); END;",
    )?;
    let baseline = counts(&runtime)?;
    runtime.record_runtime_loop_failure(&original_error()).await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    assert!(root.state.current_run_id.is_none());
    assert_eq!(
        root.state.last_runtime_failure.as_ref().unwrap().summary,
        "original loop failed"
    );
    assert_eq!(
        runtime
            .runtime_db()
            .agent_states()
            .latest(runtime.instance_agent_id())?
            .as_ref(),
        Some(root.state.as_ref())
    );
    let selected = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    assert!(!selected
        .events
        .iter()
        .any(|r| matches!(&r.fact, CurrentReadFact::Event(e)
        if e.kind == "agent_runtime_loop_failed")));
    assert_eq!(
        counts(&runtime)?,
        (
            baseline.0 + 1,
            baseline.1 + 1,
            baseline.2 + 1,
            baseline.3 + 1
        )
    );
    runtime
        .runtime_db()
        .connection()?
        .execute_batch("DROP TRIGGER reject_original_loop_failure")?;
    Ok(())
}

#[tokio::test]
async fn original_runtime_failure_source_keeps_deleting_but_has_no_stateless_or_successor_authority(
) -> Result<()> {
    for mode in ["deleting", "no_state", "retired"] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalRuntimeLoopFailureAudit { event, source } = frozen(&runtime)?;
        let weak = Arc::downgrade(&source._material);
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let p = PreparedInputTransition::runtime_loop_failure_audit(
            source.scope.clone(),
            event,
            root.state.clone(),
            source,
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
                    "UPDATE agent_states SET payload_json='poison-runtime-failure-successor' WHERE agent_id=?1",
                    [runtime.instance_agent_id()])?;
            }
        }
        let baseline = counts(&runtime)?;
        if mode == "deleting" {
            let InputMutationExecution::Committed { value, .. } = commit(&runtime, &p)? else {
                panic!("first original Runtime failure Audit replayed");
            };
            assert!(value.applied);
            drop(value);
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
                .with_context(|| format!("{mode} gained generic Runtime failure authority"))?;
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
        drop(p);
        assert!(weak.upgrade().is_none());
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
async fn original_runtime_failure_source_rejects_forgery_other_factory_and_preclone_native_budget(
) -> Result<()> {
    for mode in ["changed", "model_factory"] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let OriginalRuntimeLoopFailureAudit { mut event, source } = frozen(&runtime)?;
        let weak = Arc::downgrade(&source._material);
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let baseline = counts(&runtime)?;
        let error = if mode == "changed" {
            event.data["released_claims"] = serde_json::json!(1);
            PreparedInputTransition::runtime_loop_failure_audit(
                source.scope.clone(),
                event,
                root.state.clone(),
                source,
            )
            .err()
            .context("changed original failure Event was accepted")?
        } else {
            PreparedInputTransition::runtime_lifecycle_noop_audit(
                source.scope.clone(),
                event,
                root.state.clone(),
                source,
                None,
            )
            .err()
            .context("Model factory reused original Runtime failure Source")?
        };
        assert!(!format!("{error:#}").is_empty());
        assert_eq!(counts(&runtime)?, baseline);
        assert!(weak.upgrade().is_none());
    }
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let mut descriptor = describe_runtime_error(&original_error());
    descriptor.safe_context = (0..20000)
        .map(|i| (format!("native-key-{i}"), "value".into()))
        .collect();
    let baseline = counts(&runtime)?;
    let charged = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .prepare_original_runtime_loop_failure_audit(
            runtime.instance_agent_id(),
            "original loop failed",
            &descriptor,
            0,
        )
        .err()
        .context("Runtime failure descriptor bypassed first-copy native gate")?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(counts(&runtime)?, baseline);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        charged
    );
    Ok(())
}

#[tokio::test]
async fn original_runtime_failure_audit_unknown_ack_recovers_its_same_event_before_state_cleanup(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let OriginalRuntimeLoopFailureAudit { event, source } = frozen(&runtime)?;
    let original = (event.id.clone(), event.created_at);
    let weak = Arc::downgrade(&source._material);
    let mut owner = runtime.begin_mutation().await?;
    let root = owner.capture_published_root()?;
    let before_state = root.state.clone();
    let p = PreparedInputTransition::runtime_loop_failure_audit(
        owner.input_evidence_scope(),
        event,
        root.state.clone(),
        source,
    )?;
    drop(root);
    let baseline = counts(&runtime)?;
    let witness = crate::runtime::mutation_witness::MutationWitness::input_mutation(
        p.descriptor().command_nonce().into(),
        crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
            p.descriptor().clone(),
        ),
        Vec::new(),
    )?
    .with_input_runtime(vec![Arc::new(owner.facts(Default::default())?)])?;
    let error = owner
        .commit_transition_inner(p.descriptor().command_nonce().into(), Some(witness), || {
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Committed { .. }
            ));
            Err(anyhow!(
                "original Runtime failure Audit ACK lost after real COMMIT"
            ))
        })
        .await
        .err()
        .context("Runtime failure Audit ACK was not lost")?;
    assert!(format!("{error:#}").contains("lost after real COMMIT"));
    drop(owner);
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    assert!(reservation.recovery()?.unwrap().known.is_none());
    drop(reservation);
    let mut owner = runtime.begin_mutation().await?;
    assert_eq!(&owner.state, before_state.as_ref());
    owner.state.current_run_id = None;
    owner.current_run_abort = None;
    owner.state.last_runtime_failure = Some(RuntimeFailureSummary {
        occurred_at: original.1,
        summary: "original loop failed".into(),
        phase: RuntimeFailurePhase::RuntimeTurn,
        detail_hint: Some("the next host access will rebuild the runtime loop".into()),
        failure_artifact: None,
    });
    owner.persist_state(&runtime.inner.storage).await?;
    drop(owner);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let selected = root
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&CurrentReadRequest::current(64 * 1024 * 1024, 65536))?;
    let event = selected
        .events
        .iter()
        .find_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.id == original.0 => Some(e),
            _ => None,
        })
        .context("unknown Runtime failure Audit absent after actual recovery")?;
    assert_eq!(event.created_at, original.1);
    assert_eq!(event.kind, "agent_runtime_loop_failed");
    assert!(selected
        .events
        .iter()
        .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
        if e.kind=="agent_state_mutated" && e.event_seq==event.event_seq+1)));
    assert_eq!(
        counts(&runtime)?,
        (
            baseline.0 + 2,
            baseline.1 + 2,
            baseline.2 + 2,
            baseline.3 + 2
        )
    );
    let settled = counts(&runtime)?;
    assert!(matches!(
        commit(&runtime, &p)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(counts(&runtime)?, settled);
    drop(p);
    assert!(weak.upgrade().is_none());
    Ok(())
}
