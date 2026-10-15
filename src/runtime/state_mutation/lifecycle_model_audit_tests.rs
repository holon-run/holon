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

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "lifecycle-model-owner",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    // This real State ACK preserves the original requested-next-turn branch
    // without needing a configured external Provider or a model call.
    let mut owner = runtime.begin_mutation().await?;
    owner.state.current_run_id = Some("actual-model-existing-run".into());
    owner.persist_state(&runtime.inner.storage).await?;
    drop(owner);
    Ok((home, runtime))
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
    prepared: &PreparedInputTransition,
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
        .transitions_in_scope(&prepared.descriptor().scopes()[0])
        .commit_prepared_transition_with_work_request_and_memory_budget(
            prepared,
            &work,
            &runtime.inner.input_evidence_budget,
            &tasks,
        )
}
async fn frozen_source(
    runtime: &RuntimeHandle,
) -> Result<(AuditEvent, OriginalLifecycleNoopAuditSource)> {
    let owner = runtime.begin_mutation().await?;
    let model = runtime.model_state_for(&owner.state);
    let mut audits =
        runtime.prepare_original_model_override_audit(&owner.state.id, &model, true, true)?;
    let event = audits.events.pop().unwrap();
    let source = owner.freeze_original_model_noop_source(
        &event,
        audits._material.take().unwrap(),
        audits.material_records,
    )?;
    Ok((event, source))
}

#[tokio::test]
async fn actual_model_set_clear_keep_changed_and_true_noop_audits_in_their_original_ack(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let route = runtime
        .model_state_for(&runtime.agent_state().await?)
        .effective_model;
    let original = counts(&runtime)?;
    runtime.set_model_override(route.clone(), None).await?;
    let changed = counts(&runtime)?;
    assert_eq!(
        changed,
        (
            original.0 + 2,
            original.1 + 2,
            original.2 + 2,
            original.3 + 1
        )
    );
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.set_model_override(route, None).await?;
    let repeated = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(repeated.state, before.state);
    assert_eq!(
        repeated.current_reads.as_ref().unwrap().coverage().revision,
        before.current_reads.as_ref().unwrap().coverage().revision + 1
    );
    assert_eq!(
        counts(&runtime)?,
        (changed.0 + 1, changed.1 + 1, changed.2 + 1, changed.3 + 1)
    );
    runtime.clear_model_override().await?;
    let cleared = counts(&runtime)?;
    assert_eq!(
        cleared,
        (changed.0 + 3, changed.1 + 3, changed.2 + 3, changed.3 + 2)
    );
    let before = runtime.inner.mutation_owner.capture()?.unwrap();
    runtime.clear_model_override().await?;
    let after = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(after.state, before.state);
    assert_eq!(
        counts(&runtime)?,
        (cleared.0 + 1, cleared.1 + 1, cleared.2 + 1, cleared.3 + 1)
    );
    let selected = after.current_reads.as_ref().unwrap().capture(&request())?;
    let models = selected
        .events
        .iter()
        .filter_map(|r| match &r.fact {
            CurrentReadFact::Event(e) if e.kind.starts_with("agent_model_override_") => Some(e),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(models.len(), 4);
    assert_eq!(
        models
            .iter()
            .filter(|e| e.kind == "agent_model_override_requested")
            .count(),
        2
    );
    assert_eq!(
        models
            .iter()
            .filter(|e| e.kind == "agent_model_override_clear_requested")
            .count(),
        2
    );
    assert!(models.iter().all(|e| e.data["pending_next_turn"] == true));
    // Old Active Audit command JSON/digest remains byte compatible: both new
    // optional fields are skipped, the opaque source is never serialized.
    let event = AuditEvent::legacy("runtime_error", serde_json::json!({"old":"active"}));
    let p = PreparedInputTransition::runtime_audit(
        crate::runtime::resident_input_evidence::InputEvidenceScope {
            agent_id: runtime.instance_agent_id().into(),
            identity_incarnation: after.token.identity_incarnation,
        },
        "actual-old-active-model-companion".into(),
        event.clone(),
        after.state.clone(),
        None,
    )?;
    let old = serde_json::json!({"kind":"runtime_audit","agent_id":runtime.instance_agent_id(),"event":event});
    assert_eq!(
        p.descriptor().command_digest(),
        crate::runtime_db::input_mutation_receipts::canonical_command_digest(&old)?
    );
    Ok(())
}

#[tokio::test]
async fn actual_model_noop_rollback_known_ack_and_caller_cancel_preserve_frozen_original_event(
) -> Result<()> {
    for mode in ["normal", "rollback", "known"] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let before = runtime.inner.mutation_owner.capture()?.unwrap();
        let baseline = counts(&runtime)?;
        runtime.runtime_db().connection()?.execute_batch(
            "CREATE TRIGGER model_noop_cannot_update_state BEFORE UPDATE ON agent_states
             BEGIN SELECT RAISE(ABORT,'model noop changed State'); END;",
        )?;
        if mode == "rollback" {
            runtime.inject_next_transition_fault(TransitionFaultPoint::AfterAuditWrites);
        }
        if mode == "known" {
            runtime.inject_next_transition_fault(TransitionFaultPoint::BeforeCacheUpdate);
        }
        let outcome = runtime.clear_model_override().await;
        if mode == "rollback" {
            assert!(outcome.is_err());
            assert_eq!(counts(&runtime)?, baseline);
            let after = runtime.inner.mutation_owner.capture()?.unwrap();
            assert_eq!(after.state, before.state);
            assert!(Arc::ptr_eq(
                after.current_reads.as_ref().unwrap(),
                before.current_reads.as_ref().unwrap()
            ));
            continue;
        }
        if mode == "known" {
            assert!(outcome.is_err());
            assert!(matches!(
                runtime.inner.mutation_owner.capture(),
                Err(crate::runtime::mutation_owner::MutationOwnerError::NeedsReconcile(_))
            ));
            drop(runtime.begin_mutation().await?);
        } else {
            outcome?;
        }
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        assert_eq!(root.state, before.state);
        assert_eq!(
            root.current_reads.as_ref().unwrap().coverage().revision,
            before.current_reads.as_ref().unwrap().coverage().revision + 1
        );
        assert_eq!(
            counts(&runtime)?,
            (
                baseline.0 + 1,
                baseline.1 + 1,
                baseline.2 + 1,
                baseline.3 + 1
            )
        );
        let event: String = runtime.runtime_db().connection()?.query_row(
            "SELECT data_json FROM audit_events WHERE kind='agent_model_override_clear_requested'",
            [],
            |r| r.get(0),
        )?;
        let event: AuditEvent = serde_json::from_str(&event)?;
        let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
        assert!(selected
            .events
            .iter()
            .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
            if e.id==event.id && e.created_at==event.created_at)));
        let settled = counts(&runtime)?;
        drop(runtime.begin_mutation().await?);
        assert_eq!(counts(&runtime)?, settled);
    }
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let mut owner = runtime.begin_mutation().await?;
    let baseline = counts(&runtime)?;
    let task_count = runtime.inner.mutation_tasks.lock().unwrap().handles.len();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.clear_model_override().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.inner.mutation_tasks.lock().unwrap().handles.len() <= task_count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("Model request was not owned before waiting for its owner")?;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    // The submitted original worker may see a later normal State revision; it
    // must still complete after caller cancellation and never fake a State change.
    owner.state.turn_index += 1;
    owner.persist_state(&runtime.inner.storage).await?;
    let advanced = owner.state.clone();
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_idle_owned_tasks(),
    )
    .await??;
    assert_eq!(runtime.agent_state().await?, advanced);
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
        runtime.runtime_db().connection()?.query_row(
            "SELECT COUNT(*) FROM audit_events WHERE kind='agent_model_override_clear_requested'",
            [],
            |r| r.get::<_, usize>(0)
        )?,
        1
    );
    Ok(())
}

#[tokio::test]
async fn original_model_noop_source_rejects_foreign_epoch_ledger_event_and_survives_fresh_state(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let (event, source) = frozen_source(&runtime).await?;
    let weak = Arc::downgrade(&source._material);
    let before = counts(&runtime)?;
    let wrong = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: "foreign-model-owner".into(),
        identity_incarnation: source.scope.identity_incarnation,
    };
    assert!(source.validate_event(&wrong, &event).is_err());
    let foreign = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        crate::config::StateMemoryLimits::default(),
    )?;
    assert!(source
        .validate_accounting(&source.scope, &foreign, &event)
        .is_err());
    let mut changed = event.clone();
    changed.id.push_str("-forged");
    assert!(source.validate_event(&source.scope, &changed).is_err());
    changed = event.clone();
    changed.kind = "workspace_attached".into();
    assert!(source.validate_event(&source.scope, &changed).is_err());
    changed = event.clone();
    changed.data["override_reasoning_effort"] = serde_json::json!("forged");
    assert!(source.validate_event(&source.scope, &changed).is_err());
    let mut epoch_source = source;
    epoch_source.owner_epoch += 1;
    assert!(epoch_source.validate_owner(&runtime, &event).is_err());
    epoch_source.owner_epoch -= 1;
    assert_eq!(counts(&runtime)?, before);
    let original_id = event.id.clone();
    let original_at = event.created_at;
    {
        let mut owner = runtime.begin_mutation().await?;
        owner.state.turn_index += 1;
        owner.persist_state(&runtime.inner.storage).await?;
    }
    let advanced = runtime.agent_state().await?;
    runtime
        .finish_owned_lifecycle_noop_audit(OriginalLifecycleNoopAudit {
            event,
            source: epoch_source,
            fault: None,
        })
        .await?;
    assert_eq!(runtime.agent_state().await?, advanced);
    assert!(
        weak.upgrade().is_none(),
        "last original source material must be released after ACK"
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let selected = root.current_reads.as_ref().unwrap().capture(&request())?;
    assert!(selected
        .events
        .iter()
        .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
        if e.id==original_id && e.created_at==original_at)));
    let model = runtime.model_state_for(&advanced);
    let mut oversized = model.clone();
    oversized.override_reasoning_effort = Some("x".repeat(17 * 1024 * 1024));
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .prepare_original_model_override_audit(&advanced.id, &oversized, true, false)
        .err()
        .context("oversized borrowed Model audit incorrectly copied its body")?;
    assert!(matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn original_model_noop_preserves_only_existing_deleting_state_authority_and_exact_receipt(
) -> Result<()> {
    for mode in ["deleting", "no_state", "retired"] {
        let (_home, runtime) = fixture().await?;
        let _pause = runtime.pause_resource_observer_for_test().await;
        let (event, source) = frozen_source(&runtime).await?;
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let scope = source.scope.clone();
        let p = PreparedInputTransition::runtime_lifecycle_noop_audit(
            scope.clone(),
            event.clone(),
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
        let old_state = root.state.clone();
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
                    "UPDATE agent_states SET payload_json='poison-successor-State' WHERE agent_id=?1",
                    [runtime.instance_agent_id()])?;
            }
        }
        let baseline = counts(&runtime)?;
        if mode == "deleting" {
            let InputMutationExecution::Committed { value, .. } = commit(&runtime, &p)? else {
                panic!("first Model noop was replayed")
            };
            assert!(value.applied);
            assert_eq!(
                runtime
                    .runtime_db()
                    .agent_states()
                    .latest(runtime.instance_agent_id())?
                    .as_ref(),
                Some(old_state.as_ref())
            );
            assert_eq!(
                counts(&runtime)?,
                (
                    baseline.0 + 1,
                    baseline.1 + 1,
                    baseline.2 + 1,
                    baseline.3 + 1
                )
            );
            drop(value);
            // An ordinary Active Audit still has no Deleting authority.
            let ordinary = PreparedInputTransition::runtime_audit(
                scope.clone(),
                "ordinary-model-deleting-rejected".into(),
                AuditEvent::legacy("runtime_error", serde_json::json!({})),
                old_state.clone(),
                None,
            )?;
            let error = commit(&runtime, &ordinary)
                .err()
                .context("ordinary Active Audit gained Deleting permission")?;
            assert!(matches!(
                error.downcast_ref::<CurrentReadError>(),
                Some(CurrentReadError::RetiredScope)
            ));
            let settled = counts(&runtime)?;
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE agent_states RENAME TO model_noop_forbidden_State;
                 ALTER TABLE audit_events RENAME TO model_noop_forbidden_Audit;",
            )?;
            assert!(matches!(
                commit(&runtime, &p)?,
                InputMutationExecution::Replayed { .. }
            ));
            runtime.runtime_db().connection()?.execute_batch(
                "ALTER TABLE model_noop_forbidden_State RENAME TO agent_states;
                 ALTER TABLE model_noop_forbidden_Audit RENAME TO audit_events;",
            )?;
            assert_eq!(counts(&runtime)?, settled);
        } else {
            let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
            let error = commit(&runtime, &p)
                .err()
                .with_context(|| format!("Model source {mode} gained permission"))?;
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
                before
            );
        }
        runtime
            .runtime_db()
            .agent_identities()
            .upsert(&old_identity)?;
        runtime
            .runtime_db()
            .transaction(|tx| crate::runtime_db::evidence::upsert_agent_state_tx(tx, &old_state))?;
    }
    Ok(())
}

#[tokio::test]
async fn actual_owned_model_request_finishes_during_closing_and_new_public_request_rejects(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let owner = runtime.begin_mutation().await?;
    let before = counts(&runtime)?;
    let original_state = owner.state.clone();
    let task_count = runtime.inner.mutation_tasks.lock().unwrap().handles.len();
    let caller = tokio::spawn({
        let runtime = runtime.clone();
        async move { runtime.clear_model_override().await }
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while runtime.inner.mutation_tasks.lock().unwrap().handles.len() <= task_count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("original Model request was not registered")?;
    runtime.close_mutation_admission();
    assert!(
        runtime.clear_model_override().await.is_err(),
        "Closing accepted a new Model request"
    );
    assert_eq!(counts(&runtime)?, before);
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(owner);
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.join_owned_mutations(),
    )
    .await??;
    assert_eq!(runtime.agent_state().await?, original_state);
    assert_eq!(
        counts(&runtime)?,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    Ok(())
}

#[tokio::test]
async fn original_model_noop_lost_ack_recovers_only_the_frozen_whole_receipt_without_reexecution(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let (event, source) = frozen_source(&runtime).await?;
    let original_id = event.id.clone();
    let original_time = event.created_at;
    let before = counts(&runtime)?;
    let mut owner = runtime.begin_mutation().await?;
    let original_state = owner.state.clone();
    let root = owner.capture_published_root()?;
    let p = PreparedInputTransition::runtime_lifecycle_noop_audit(
        owner.input_evidence_scope(),
        event,
        root.state.clone(),
        source,
        None,
    )?;
    drop(root);
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
            let actual = commit(&runtime, &p)?;
            assert!(
                matches!(actual, InputMutationExecution::Committed { .. }),
                "Model SQL must commit before ACK is lost"
            );
            Err(anyhow!("original Model noop ACK lost after real COMMIT"))
        })
        .await
        .err()
        .context("Model noop original ACK was not lost")?;
    assert!(format!("{error:#}").contains("lost after real COMMIT"));
    drop(owner);
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    assert!(reservation.recovery()?.unwrap().known.is_none());
    drop(reservation);
    let committed = counts(&runtime)?;
    assert_eq!(
        committed,
        (before.0 + 1, before.1 + 1, before.2 + 1, before.3 + 1)
    );
    drop(runtime.begin_mutation().await?);
    let restored = runtime.inner.mutation_owner.capture()?.unwrap();
    assert_eq!(restored.state.as_ref(), &original_state);
    let selected = restored
        .current_reads
        .as_ref()
        .unwrap()
        .capture(&request())?;
    assert!(selected
        .events
        .iter()
        .any(|r| matches!(&r.fact,CurrentReadFact::Event(e)
        if e.id==original_id && e.created_at==original_time)));
    assert_eq!(counts(&runtime)?, committed);
    assert!(matches!(
        commit(&runtime, &p)?,
        InputMutationExecution::Replayed { .. }
    ));
    assert_eq!(counts(&runtime)?, committed);
    Ok(())
}

#[tokio::test]
async fn original_model_source_actual_guard_and_sql_each_fit_but_joint_phase_rejects_before_receipt_read(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let (event, mut source) = frozen_source(&runtime).await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let scope = source.scope.clone();
    // The unchanged original SQL actor is independently feasible. Its ACK also
    // makes this exact Event a real qualified noop for the focused source.
    let ordinary = PreparedInputTransition::runtime_audit(
        scope.clone(),
        "actual-model-source-independent-sql".into(),
        event.clone(),
        root.state.clone(),
        None,
    )?;
    let InputMutationExecution::Committed { value, .. } = commit(&runtime, &ordinary)? else {
        panic!("independent actual Model Audit SQL unexpectedly replayed")
    };
    assert!(value.applied);
    drop(value);
    let cap = 64 * 1024 * 1024usize;
    let original_cost = usize::try_from(source._material.owned_material_bytes()?)?;
    assert!(original_cost < cap - 1);
    // A real retained Vec plus the original source share one actual accounting
    // view in this private material fixture; no public constructor can forge it.
    // The original business source/Event/owner proof remains unchanged.
    let combined = runtime.inner.input_evidence_budget.register_owned_bytes(
        &scope,
        crate::runtime::input_evidence_budget::InputMemoryCategory::Candidate,
        crate::runtime::input_evidence_budget::OwnedBytesEstimate {
            payload_bytes: u64::try_from(cap - 1)?,
            metadata_estimate_bytes: 0,
        },
    )?;
    let held = vec![0u8; cap - 1 - original_cost - 256];
    source._material = combined;
    source.material_records = source.material_records.checked_add(1).unwrap();
    let cost = source.material_prior(&scope, &runtime.inner.input_evidence_budget, &event)?;
    request().check_material(cost.0, cost.1)?;
    assert_eq!(cost.0, cap - 1);
    let weak = Arc::downgrade(&source._material);
    let p = PreparedInputTransition::runtime_lifecycle_noop_audit(
        scope,
        event,
        root.state.clone(),
        source,
        None,
    )?;
    let before = counts(&runtime)?;
    let coverage: String = runtime.runtime_db().connection()?.query_row(
        "SELECT coverage_json FROM current_read_current WHERE agent_id=?1",
        [runtime.instance_agent_id()],
        |r| r.get(0),
    )?;
    let charged = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE input_mutation_receipts RENAME TO model_source_forbidden_receipts;",
    )?;
    let error = commit(&runtime, &p)
        .err()
        .context("source and SQL reset the original joint phase cap")?;
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE model_source_forbidden_receipts RENAME TO input_mutation_receipts;",
    )?;
    assert!(
        matches!(
            error.downcast_ref::<CurrentReadError>(),
            Some(CurrentReadError::ResourceExhausted)
        ),
        "{error:#}"
    );
    assert_eq!(counts(&runtime)?, before);
    assert_eq!(
        runtime.runtime_db().connection()?.query_row(
            "SELECT coverage_json FROM current_read_current WHERE agent_id=?1",
            [runtime.instance_agent_id()],
            |r| r.get::<_, String>(0)
        )?,
        coverage
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        charged
    );
    assert_eq!(held.len() + original_cost + 256, cap - 1);
    assert!(weak.upgrade().is_some());
    drop((p, ordinary, held));
    assert!(weak.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}
