//! Actual Work ACKs and selected resource observations; no synthetic Ready.
use super::*;
use crate::runtime::tests::support::{bind_turn_to_work_item, context_config, tempdir};
use crate::types::{WorkItemPlanStatus, WorkItemState};

fn runtime(home: &Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new_isolated_resource_fixture(
        "resource-work",
        home.to_path_buf(),
        context_config(),
    )
}
async fn item(runtime: &RuntimeHandle, plan: &str) -> Result<crate::types::WorkItemRecord> {
    runtime
        .create_work_item(
            "observed Work".into(),
            Some(WorkItemPlanStatus::Ready),
            Some(plan.into()),
            Vec::new(),
        )
        .await
}

#[tokio::test]
async fn metadata_work_acks_rebind_only_header_and_keep_observed_plan_without_file_reads(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "original observed plan").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let before = runtime.capture_current_context_resources()?;
    let selected = runtime.capture_observed_work_plan(&work.id, work.revision)?;
    // This external edit has not been observed; an ordinary writer must use the
    // same immutable observation rather than silently read ahead on the disk.
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &work.id),
        "not observed yet",
    )?;
    let blocked = crate::diagnostics::resource_io::deny_readers(
        runtime.update_work_item_fields_with_recheck(
            work.id.clone(),
            Some("updated objective".into()),
            None,
            None,
            None,
            Some(Some("waiting for original reply".into())),
            Some(1),
        ),
    )
    .await?;
    assert_eq!(blocked.plan_artifact, work.plan_artifact);
    let consumed =
        crate::diagnostics::resource_io::deny_readers(runtime.consume_work_item_recheck(&work.id))
            .await?
            .context("actual recheck not consumed")?;
    assert!(consumed.recheck_consumed_at.is_some());
    assert_eq!(consumed.plan_artifact, work.plan_artifact);
    let after = runtime.capture_current_context_resources()?;
    assert!(Arc::ptr_eq(
        &before.observation.body,
        &after.observation.body
    ));
    assert_ne!(
        before.observation.source.work_revision,
        after.observation.source.work_revision
    );
    let after_selected = runtime.capture_observed_work_plan(&work.id, consumed.revision)?;
    assert!(Arc::ptr_eq(&selected.plan, &after_selected.plan));
    let picked = crate::diagnostics::resource_io::deny_readers(
        runtime.pick_work_item_with_reason_and_clear_blocker(
            work.id.clone(),
            Some("original wait cleared".into()),
            true,
        ),
    )
    .await?;
    assert!(picked.current_work_item.blocked_by.is_none());
    assert_eq!(picked.current_work_item.plan_artifact, work.plan_artifact);
    drop(pause);
    runtime.refresh_context_resources().await?;
    let actual = runtime.latest_work_item(&work.id).await?.unwrap();
    let fresh = runtime.capture_observed_work_plan(&work.id, actual.revision)?;
    let mut record = actual.clone();
    assert!(fresh.apply(&mut record)?);
    assert_eq!(
        record.plan_artifact.as_ref().unwrap().preview,
        "not observed yet"
    );
    assert_eq!(selected.target().plan_artifact, work.plan_artifact);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn completing_and_turn_end_use_actual_observed_plan_with_file_readers_denied() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let first = item(&runtime, "completion observation").await?;
    let second = item(&runtime, "turn end observation").await?;
    bind_turn_to_work_item(&runtime, &second.id).await;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let original_resources = runtime.capture_current_context_resources()?;
    let completed = crate::diagnostics::resource_io::deny_readers(
        runtime.complete_work_item(first.id.clone(), Vec::new()),
    )
    .await
    .with_context(|| {
        format!(
            "complete intent; resource={:?}",
            runtime.inner.context_resources.freshness()
        )
    })?;
    assert_eq!(completed.state, WorkItemState::Completing);
    assert_eq!(completed.plan_artifact, first.plan_artifact);
    let promotion = crate::diagnostics::resource_io::deny_readers(
        runtime.promote_work_item_completion_report_with_metadata(
            first.id.clone(),
            "actual completion report".into(),
            Vec::new(),
            None,
            None,
            Vec::new(),
        ),
    )
    .await
    .with_context(|| {
        format!(
            "promote report; resource={:?}",
            runtime.inner.context_resources.freshness()
        )
    })?
    .into_record();
    assert_eq!(promotion.state, WorkItemState::Completed);
    assert_eq!(promotion.plan_artifact, first.plan_artifact);
    let committed = crate::diagnostics::resource_io::deny_readers(
        runtime.maybe_commit_turn_end_work_item_transition(),
    )
    .await
    .with_context(|| {
        format!(
            "turn end; resource={:?}",
            runtime.inner.context_resources.freshness()
        )
    })?
    .context("turn end Work missing")?;
    assert_eq!(committed.id, second.id);
    assert_eq!(committed.plan_artifact, second.plan_artifact);
    let after = runtime.capture_current_context_resources()?;
    assert!(
        Arc::ptr_eq(
            &original_resources.observation.body,
            &after.observation.body
        ),
        "named metadata preparation reuses the original observed core body"
    );
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn selected_plan_and_guard_outlive_registry_until_the_actual_last_carrier_drop() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "retained selected plan").await?;
    runtime.refresh_context_resources().await?;
    let selected = runtime.capture_observed_work_plan(&work.id, work.revision)?;
    let plan = Arc::downgrade(&selected.plan);
    let material = Arc::downgrade(&selected._plan_material);
    runtime.stop_context_resource_observer().await?;
    assert!(plan.upgrade().is_some());
    assert!(material.upgrade().is_some());
    let mut copy = work;
    assert!(!selected.apply(&mut copy)?);
    assert_eq!(
        copy.plan_artifact.as_ref().unwrap().preview,
        "retained selected plan"
    );
    drop(selected);
    assert!(plan.upgrade().is_none());
    assert!(material.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn recorded_missing_plan_rejects_work_write_without_creating_a_replacement() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "original required artifact").await?;
    let path = crate::work_item_plan::plan_path(home.path(), &work.id);
    std::fs::remove_file(&path)?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let selected = runtime.capture_observed_work_plan(&work.id, work.revision)?;
    assert!(matches!(
        selected.plan.as_ref(),
        ObservedPlan::RecordedMissing
    ));
    let error = crate::diagnostics::resource_io::deny_readers(runtime.update_work_item_fields(
        work.id.clone(),
        Some("must not commit".into()),
        None,
        None,
        None,
        None,
    ))
    .await
    .unwrap_err();
    assert!(error.to_string().contains("missing plan artifact"));
    assert!(!path.exists());
    assert_eq!(runtime.latest_work_item(&work.id).await?.unwrap(), work);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn original_qualified_legacy_work_uses_explicit_first_file_workflow_not_capture() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "old artifact removed by fixture").await?;
    let mut legacy = work.clone();
    legacy.plan_artifact = None;
    legacy.revision += 1;
    let commit = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .commit_work_transition(
                crate::ids::audit_event_id(),
                crate::runtime_db::transitions::WorkItemTransitionCommand {
                    agent_id: legacy.agent_id.clone(),
                    mutation: crate::runtime_db::transitions::WorkItemMutation::Update {
                        record: legacy.clone(),
                        expected_revision: work.revision,
                    },
                    agent_state: None,
                    brief_evidence: Vec::new(),
                    audit_events: Vec::new(),
                    index_changes: Vec::new(),
                    notify_scheduler: true,
                    fault: None,
                },
                Default::default(),
                true,
            )
            .await?
    };
    runtime.apply_transition_commit(commit).await;
    let path = crate::work_item_plan::plan_path(home.path(), &work.id);
    std::fs::remove_file(&path)?;
    runtime.refresh_context_resources().await?;
    let capture = crate::diagnostics::resource_io::deny_readers(async {
        runtime.capture_observed_work_plan(&legacy.id, legacy.revision)
    })
    .await?;
    assert!(capture.requires_first_file());
    assert!(!path.exists());
    // This is the actual existing Work/file workflow, with no held owner.
    let prepared = runtime
        .prepare_observed_work_plan(&legacy.id, legacy.revision)
        .await?;
    assert!(path.is_file());
    assert!(!prepared.requires_first_file());
    let mut candidate = legacy.clone();
    assert!(prepared.apply(&mut candidate)?);
    let updated = runtime
        .update_work_item_fields(
            legacy.id.clone(),
            Some("legacy observed".into()),
            None,
            None,
            None,
            None,
        )
        .await?;
    assert_eq!(updated.plan_artifact, candidate.plan_artifact);
    assert!(runtime
        .capture_observed_work_plan(&legacy.id, legacy.revision)
        .is_err());
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn prepared_wait_holds_selected_plan_until_actual_wait_ack_and_last_prepared_drop(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "original Wait observation").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let outcome = crate::diagnostics::resource_io::deny_readers(runtime.prepare_wait_for_outcome(
        "resource-work",
        Some(work.id.clone()),
        WaitForWakeKind::OperatorInput,
        None,
        "original operator decision".into(),
        Some(100),
    ))
    .await?;
    let crate::runtime::waiting::PrepareWaitForOutcome::Prepared(prepared) = outcome else {
        anyhow::bail!("actual ordinary Wait was not prepared")
    };
    let plan = runtime
        .inner
        .context_resources
        .plan_weak_for_test(&work.id)?
        .unwrap();
    assert!(plan.upgrade().is_some());
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    assert!(
        plan.upgrade().is_some(),
        "original prepared Wait owns selected plan"
    );
    let commit = {
        let mut owner = runtime.begin_mutation().await?;
        crate::diagnostics::resource_io::deny_readers(owner.commit_wait_transition(
            crate::ids::audit_event_id(),
            prepared.command.clone(),
            prepared.execution_protocol.clone(),
            prepared.expected_task.clone(),
        ))
        .await?
    };
    runtime.apply_transition_commit(commit).await;
    let current = runtime.latest_work_item(&work.id).await?.unwrap();
    assert_eq!(
        current.blocked_by_wait_id.as_deref(),
        Some(prepared.registration.condition.id.as_str())
    );
    assert_eq!(current.plan_artifact, work.plan_artifact);
    assert!(plan.upgrade().is_some());
    drop(prepared);
    assert!(plan.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn actual_scheduler_prior_source_and_plan_share_one_remaining_allowance_before_clones(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "selected material observation").await?;
    let read = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?
    };
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let selected = runtime.capture_observed_work_plan_with_prior(
        &work.id,
        work.revision,
        Some(crate::runtime::resource_refresh::WorkPlanReadPrior::new(
            &read.state,
            &read.work,
            &read.resource_stamp,
            read.material_estimate()?,
        )),
    )?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let source = runtime.context_resource_source(&root.state, root.work.as_deref())?;
    let prior = read.material_estimate()?;
    selected.validate_material_binding(&read.work.scope, &runtime.inner.input_evidence_budget)?;
    let target = read
        .work
        .records
        .iter()
        .find(|record| selected.shares_work_allocation(record))
        .context("selected Work must share the original qualified allocation")?;
    let (work_bytes, work_records) = target.record.resident_material_estimate()?;
    let selected_material = selected.material_estimate()?;
    let mut foreign_scope = selected.scope().clone();
    foreign_scope.identity_incarnation = foreign_scope.identity_incarnation.checked_add(1).unwrap();
    assert!(selected
        .validate_material_binding(&foreign_scope, &runtime.inner.input_evidence_budget)
        .is_err());
    let foreign_ledger = crate::runtime::input_evidence_budget::InputEvidenceBudgetLedger::new(
        runtime.inner.input_evidence_budget.limits(),
    )?;
    assert!(selected
        .validate_material_binding(selected.scope(), &foreign_ledger)
        .is_err());
    let plan_material =
        crate::runtime::resource_observation::plan_material(&work.id, selected.plan.as_ref())?;
    assert_eq!(
        selected_material,
        (
            u64::try_from(work_bytes)?
                .checked_add(source.bytes)
                .unwrap()
                .checked_add(plan_material.0)
                .unwrap(),
            work_records
                .checked_add(source.records)
                .unwrap()
                .checked_add(plan_material.1)
                .unwrap(),
        )
    );
    let limits = runtime.resource_material_limits()?;
    // Every component fits alone; only the second selected-plan acquisition
    // exhausts the actual same phase's remaining bytes/records.
    let bytes = prior
        .0
        .checked_add(source.bytes)
        .unwrap()
        .checked_add(plan_material.0)
        .unwrap()
        - 1;
    let records = prior
        .1
        .checked_add(source.records)
        .unwrap()
        .checked_add(plan_material.1)
        .unwrap()
        - 1;
    assert!(bytes < limits.1 && records < limits.0);
    assert!(prior.0 <= bytes && source.bytes + plan_material.0 <= bytes);
    assert!(prior.1 <= records && source.records + plan_material.1 <= records);
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    for limit in [(limits.0, bytes), (records, limits.1)] {
        let error = runtime
            .capture_observed_work_plan_with_limits(
                &work.id,
                work.revision,
                Some(crate::runtime::resource_refresh::WorkPlanReadPrior::new(
                    &read.state,
                    &read.work,
                    &read.resource_stamp,
                    prior,
                )),
                limit,
            )
            .unwrap_err();
        assert!(
            matches!(
                error.downcast_ref::<ResourceObservationError>(),
                Some(ResourceObservationError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            before
        );
        assert!(Arc::ptr_eq(
            &root,
            &runtime.inner.mutation_owner.capture()?.unwrap()
        ));
    }
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn original_scheduler_read_rejects_successor_work_source_with_unchanged_target_record(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let target = item(&runtime, "unchanged target source").await?;
    let other = item(&runtime, "other original Work").await?;
    runtime.refresh_context_resources().await?;
    let read = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?
    };
    let original = read
        .work
        .records
        .iter()
        .find(|record| {
            matches!(&record.record,
        CanonicalWorkRecord::Work(work) if work.id==target.id)
        })
        .context("original target missing")?
        .clone();
    let pause = runtime.pause_resource_observer_for_test().await;
    runtime
        .update_work_item_fields(
            other.id.clone(),
            Some("other Work actual ACK".into()),
            None,
            None,
            None,
            None,
        )
        .await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let current = root
        .work
        .as_ref()
        .unwrap()
        .qualified_context_plan_source(&target.id)
        .unwrap();
    // The current qualified loader can budget and rebuild an unchanged body.
    // Preserve the actual record/revision, without inventing Arc reuse.
    assert_eq!(original.as_ref(), current.as_ref());
    assert!(matches!(&current.record, CanonicalWorkRecord::Work(work)
        if work.id==target.id && work.revision==target.revision));
    assert_ne!(
        root.work.as_ref().unwrap().coverage().revision,
        read.work.revision
    );
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    for prior in [WorkPlanReadPrior::new(
        &read.state,
        &read.work,
        &read.resource_stamp,
        read.material_estimate()?,
    )] {
        let error = runtime
            .capture_observed_work_plan_with_prior(&target.id, target.revision, Some(prior))
            .unwrap_err();
        assert!(
            error.downcast_ref::<WorkPlanReplanRequired>().is_some(),
            "{error:#}"
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            before
        );
    }
    let fresh = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?
    };
    let selected = runtime.capture_observed_work_plan_with_prior(
        &target.id,
        target.revision,
        Some(WorkPlanReadPrior::new(
            &fresh.state,
            &fresh.work,
            &fresh.resource_stamp,
            fresh.material_estimate()?,
        )),
    )?;
    let fresh_target = fresh
        .work
        .records
        .iter()
        .find(|qualified| {
            matches!(&qualified.record,
        CanonicalWorkRecord::Work(work) if work.id==target.id)
        })
        .context("fresh target missing")?;
    assert!(selected.shares_work_allocation(fresh_target));
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn original_work_plan_stamp_rejects_state_and_real_config_changes_without_target_revision_drift(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let target = item(&runtime, "original config source").await?;
    runtime.refresh_context_resources().await?;
    let before_state = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?
    };
    runtime.set_timezone_override(Some("UTC".into())).await?;
    let error = runtime
        .capture_observed_work_plan_with_prior(
            &target.id,
            target.revision,
            Some(WorkPlanReadPrior::new(
                &before_state.state,
                &before_state.work,
                &before_state.resource_stamp,
                before_state.material_estimate()?,
            )),
        )
        .unwrap_err();
    assert!(
        error.downcast_ref::<WorkPlanReplanRequired>().is_some(),
        "{error:#}"
    );
    let before_config = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?
    };
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let original_target = before_config
        .work
        .records
        .iter()
        .find(|qualified| {
            matches!(&qualified.record,
        CanonicalWorkRecord::Work(work) if work.id==target.id)
        })
        .context("original config target missing")?;
    let mut config =
        crate::config::AppConfig::load_with_home(Some(home.path().join("config-home")))?;
    config
        .providers
        .get_mut(&crate::config::ProviderId::openai())
        .context("fixture provider missing")?
        .credential = Some("resource-fixture-no-wire-send".into());
    runtime.reload_config(&config).await?;
    assert!(
        Arc::ptr_eq(&root, &runtime.inner.mutation_owner.capture()?.unwrap()),
        "config invalidation must be independently tested without a new business root"
    );
    assert!(
        Arc::ptr_eq(
            original_target,
            runtime
                .inner
                .mutation_owner
                .capture()?
                .unwrap()
                .work
                .as_ref()
                .unwrap()
                .qualified_context_plan_source(&target.id)
                .context("actual config target missing")?
        ),
        "real configuration source change preserves the exact original target Arc"
    );
    let error = runtime
        .capture_observed_work_plan_with_prior(
            &target.id,
            target.revision,
            Some(WorkPlanReadPrior::new(
                &before_config.state,
                &before_config.work,
                &before_config.resource_stamp,
                before_config.material_estimate()?,
            )),
        )
        .unwrap_err();
    assert!(
        error.downcast_ref::<WorkPlanReplanRequired>().is_some(),
        "{error:#}"
    );
    runtime.refresh_context_resources().await?;
    let fresh = {
        let mut owner = runtime.begin_mutation().await?;
        owner
            .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
            .await?
    };
    runtime.capture_observed_work_plan_with_prior(
        &target.id,
        target.revision,
        Some(WorkPlanReadPrior::new(
            &fresh.state,
            &fresh.work,
            &fresh.resource_stamp,
            fresh.material_estimate()?,
        )),
    )?;
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn actual_config_reload_after_initial_source_check_and_during_capture_rejects_original_read(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "frozen original source").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    for during_capture in [false, true] {
        let read = {
            let mut owner = runtime.begin_mutation().await?;
            owner
                .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
                .await?
        };
        let root = runtime.inner.mutation_owner.capture()?.unwrap();
        let source = runtime.context_resource_source(&root.state, root.work.as_deref())?;
        // Config reload's existing best-effort auto-refresh must defer while
        // this real owner is held. Release it before the explicit sync kernel.
        let mut held = Some(runtime.begin_mutation().await?);
        let mut config =
            crate::config::AppConfig::load_with_home(Some(home.path().join("config-race")))?;
        config
            .providers
            .get_mut(&crate::config::ProviderId::openai())
            .context("fixture provider missing")?
            .credential = Some("resource-fixture-no-wire-send".into());
        let start = Arc::new(std::sync::Barrier::new(2));
        let end = Arc::new(std::sync::Barrier::new(2));
        let worker_runtime = runtime.clone();
        let handle = tokio::runtime::Handle::current();
        let worker_start = start.clone();
        let worker_end = end.clone();
        let worker = std::thread::spawn(move || {
            worker_start.wait();
            let result = handle.block_on(worker_runtime.reload_config(&config));
            worker_end.wait();
            result
        });
        let prior = WorkPlanReadPrior::new(
            &read.state,
            &read.work,
            &read.resource_stamp,
            read.material_estimate()?,
        );
        let mut baseline = None;
        let mut failed_guards = None;
        let mut reload = || -> Result<()> {
            start.wait();
            end.wait();
            drop(held.take());
            // The actual observer coordinator is paused by this task; use its
            // original synchronous kernel with no owner or writer TX held.
            runtime.refresh_context_resources_sync()?;
            baseline = Some(runtime.inner.input_evidence_budget.report()?.observed_bytes);
            Ok(())
        };
        let error = if during_capture {
            runtime
                .with_original_work_plan_fence(&source.source, Some(prior), || {
                    let plan = runtime.inner.context_resources.capture_work_plan(
                        &source.source,
                        &work.id,
                        &runtime.inner.input_evidence_budget,
                        64 * 1024 * 1024,
                        65_536,
                    )?;
                    failed_guards = Some((Arc::downgrade(&plan.1), Arc::downgrade(&plan.2)));
                    reload()?;
                    Ok(plan)
                })
                .err()
                .context("configuration changed during selected capture must reject")?
        } else {
            runtime
                .capture_observed_work_plan_after_validation(
                    &work.id,
                    work.revision,
                    Some(prior),
                    runtime.resource_material_limits()?,
                    reload,
                )
                .err()
                .context("configuration changed after original check must reject")?
        };
        worker.join().expect("real configuration worker panicked")?;
        assert!(
            error.downcast_ref::<WorkPlanReplanRequired>().is_some(),
            "{error:#}"
        );
        assert!(
            Arc::ptr_eq(&root, &runtime.inner.mutation_owner.capture()?.unwrap()),
            "only the actual resource configuration changed"
        );
        if let Some((selected, original_refresh)) = failed_guards {
            assert!(selected.upgrade().is_none());
            assert!(original_refresh.upgrade().is_none());
            assert!(
                runtime.inner.input_evidence_budget.report()?.observed_bytes < baseline.unwrap(),
                "the failed candidate's actual original guards must drop"
            );
        } else {
            assert_eq!(
                runtime.inner.input_evidence_budget.report()?.observed_bytes,
                baseline.unwrap(),
                "failed newly allocated source guard must release"
            );
        }
    }
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}
