//! Actual Tool Get/List, immutable selected observations, and explicit recall.
use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::types::{AuthorityClass, WorkItemPlanStatus};

fn runtime(home: &Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new_isolated_resource_fixture(
        "resource-query",
        home.to_path_buf(),
        context_config(),
    )
}
async fn item(runtime: &RuntimeHandle, plan: &str) -> Result<WorkItemRecord> {
    runtime
        .create_work_item(
            "query observed Work".into(),
            Some(WorkItemPlanStatus::Ready),
            Some(plan.into()),
            Vec::new(),
        )
        .await
}
async fn get(runtime: &RuntimeHandle, id: &str) -> Result<serde_json::Value> {
    Ok(crate::tool::tools::get_work_item::execute(
        runtime,
        runtime.instance_agent_id(),
        &AuthorityClass::OperatorInstruction,
        &serde_json::json!({"work_item_id":id,"include_todo_list":true}),
    )
    .await?
    .envelope
    .result
    .context("Get result absent")?)
}
async fn list(runtime: &RuntimeHandle) -> Result<serde_json::Value> {
    Ok(crate::tool::tools::list_work_items::execute(
        runtime,
        runtime.instance_agent_id(),
        &AuthorityClass::OperatorInstruction,
        &serde_json::json!({"filter":"all","limit":100}),
    )
    .await?
    .envelope
    .result
    .context("List result absent")?)
}

#[tokio::test]
async fn actual_work_tools_use_latest_observed_plan_without_reading_files() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let first = item(&runtime, "original observed plan").await?;
    let second = item(&runtime, "second observed plan").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let before = crate::diagnostics::resource_io::deny_readers(get(&runtime, &first.id)).await?;
    let original_list = crate::diagnostics::resource_io::deny_readers(list(&runtime)).await?;
    assert_eq!(
        before["work_item"]["plan_artifact"]["preview"],
        "original observed plan"
    );
    assert_eq!(original_list["returned"], 2);
    assert_eq!(original_list["total_matching"], 2);
    let ordered = original_list["work_items"]
        .as_array()
        .context("List array absent")?
        .iter()
        .map(|value| value["id"].as_str().unwrap())
        .collect::<Vec<_>>();
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let expected = snapshot
        .items
        .iter()
        .map(|value| value.work_item.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ordered, expected);
    assert!(ordered.contains(&second.id.as_str()));
    assert!(before["work_item"]["plan"].is_null());
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &first.id),
        "external edit still unobserved",
    )?;
    let old = crate::diagnostics::resource_io::deny_readers(get(&runtime, &first.id)).await?;
    assert_eq!(old, before);
    drop(snapshot);
    drop(pause);
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let fresh = crate::diagnostics::resource_io::deny_readers(get(&runtime, &first.id)).await?;
    assert_eq!(
        fresh["work_item"]["plan_artifact"]["preview"],
        "external edit still unobserved"
    );
    assert_ne!(
        fresh["work_item"]["plan_artifact"]["hash"],
        before["work_item"]["plan_artifact"]["hash"]
    );
    assert_eq!(fresh["work_item"]["id"], before["work_item"]["id"]);
    assert_eq!(runtime.latest_work_item(&first.id).await?.unwrap(), first);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn query_missing_and_legacy_absent_plans_keep_original_views_without_creating_files(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let missing = item(&runtime, "recorded plan removed").await?;
    let old = item(&runtime, "legacy plan removed").await?;
    let mut legacy = old.clone();
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
                        expected_revision: old.revision,
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
    let missing_path = crate::work_item_plan::plan_path(home.path(), &missing.id);
    let absent_path = crate::work_item_plan::plan_path(home.path(), &legacy.id);
    std::fs::remove_file(&missing_path)?;
    std::fs::remove_file(&absent_path)?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let missing_view =
        crate::diagnostics::resource_io::deny_readers(get(&runtime, &missing.id)).await?;
    let absent_view =
        crate::diagnostics::resource_io::deny_readers(get(&runtime, &legacy.id)).await?;
    let listed = crate::diagnostics::resource_io::deny_readers(list(&runtime)).await?;
    assert_eq!(missing_view["work_item"]["plan_artifact_status"], "missing");
    assert!(missing_view["work_item"]["plan_artifact"].is_null());
    assert!(missing_view["work_item"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str().unwrap().contains("plan artifact missing")));
    assert_eq!(
        absent_view["work_item"]["plan_artifact_status"],
        "not_recorded"
    );
    assert!(absent_view["work_item"]["plan_artifact"].is_null());
    assert_eq!(listed["returned"], 2);
    assert!(!missing_path.exists());
    assert!(!absent_path.exists());
    assert_eq!(
        runtime.latest_work_item(&missing.id).await?.unwrap(),
        missing
    );
    assert_eq!(runtime.latest_work_item(&legacy.id).await?.unwrap(), legacy);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn query_freezes_resource_version_and_retains_selected_material_through_actual_wire(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, "original retained query plan").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let read = runtime.capture_work_item_plan_query(&snapshot, &work)?;
    let selected = Arc::downgrade(read._plan.as_ref().unwrap());
    let material = Arc::downgrade(&read._material);
    let projection = snapshot
        .items
        .iter()
        .find(|value| value.work_item.id == work.id)
        .unwrap()
        .clone();
    let context = crate::tool::tools::work_item_query::query_context_from_work_items(
        &snapshot.state,
        &snapshot.items,
    );
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &work.id),
        "new observation",
    )?;
    drop(pause);
    runtime.refresh_context_resources().await?;
    let error = runtime
        .capture_work_item_plan_query(&snapshot, &work)
        .err()
        .context("old query version accepted successor plan")?;
    assert!(matches!(
        error.downcast_ref::<ResourceObservationError>(),
        Some(ResourceObservationError::ObsoleteRefresh)
    ));
    runtime.stop_context_resource_observer().await?;
    drop(snapshot);
    assert!(selected.upgrade().is_some());
    assert!(material.upgrade().is_some());
    let (view, wire_material) = crate::tool::tools::work_item_query::view_for_record_observed(
        &runtime, work, false, None, projection, read,
    )?;
    assert_eq!(
        view.plan_artifact.as_ref().unwrap().preview,
        "original retained query plan"
    );
    let wire = crate::tool::tools::serialize_success(
        "GetWorkItem",
        &crate::tool::tools::get_work_item::GetWorkItemResult {
            context,
            work_item: view,
        },
    )?;
    assert_eq!(
        wire.envelope.result.as_ref().unwrap()["work_item"]["plan_artifact"]["preview"],
        "original retained query plan"
    );
    assert!(selected.upgrade().is_some());
    assert!(material.upgrade().is_some());
    drop(wire_material);
    assert!(selected.upgrade().is_none());
    assert!(material.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn work_query_prior_view_and_plan_share_one_allowance_and_failed_capture_releases_guards(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let work = item(&runtime, &"bounded selected plan ".repeat(80)).await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    for record_limit in [false, true] {
        let mut snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
        let source = Arc::get_mut(snapshot.resource_source.as_mut().unwrap()).unwrap();
        let original_limits = source.limits;
        let prior = *source.usage.lock().unwrap();
        let view = view_material(&work)?;
        let plan = crate::runtime::resource_observation::plan_material(
            &work.id,
            &ObservedPlan::Present(work.plan_artifact.clone().unwrap()),
        )?;
        let combined = (prior.0 + view.0 + plan.0, prior.1 + view.1 + plan.1);
        if record_limit {
            source.limits.1 = combined.1 - 1;
            assert!(prior.1 < source.limits.1);
            assert!(view.1 + plan.1 < source.limits.1);
        } else {
            source.limits.0 = combined.0 - 1;
            assert!(prior.0 < source.limits.0);
            assert!(view.0 + plan.0 < source.limits.0);
        }
        let root = runtime
            .inner
            .mutation_owner
            .capture()?
            .context("published Root absent")?;
        let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = runtime
            .capture_work_item_plan_query(&snapshot, &work)
            .err()
            .context("combined query over-budget accepted")?;
        assert!(matches!(
            error.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::ResourceExhausted(_))
        ));
        assert_eq!(
            *snapshot
                .resource_source
                .as_ref()
                .unwrap()
                .usage
                .lock()
                .unwrap(),
            prior
        );
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            before
        );
        assert!(Arc::ptr_eq(
            &root,
            &runtime
                .inner
                .mutation_owner
                .capture()?
                .context("published Root absent")?
        ));
        Arc::get_mut(snapshot.resource_source.as_mut().unwrap())
            .unwrap()
            .limits = original_limits;
        let read = runtime.capture_work_item_plan_query(&snapshot, &work)?;
        assert_eq!(
            read.query,
            PlanArtifactQuery::Available(work.plan_artifact.clone().unwrap())
        );
    }
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn foreign_by_id_work_plan_inspection_keeps_current_home_and_does_not_adopt_foreign_work(
) -> Result<()> {
    let home = tempdir()?;
    let foreign_home = tempdir()?;
    let runtime = runtime(home.path())?;
    let foreign = RuntimeHandle::new_offline_with_runtime_db(
        "foreign-query",
        foreign_home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        runtime.inner.runtime_db.clone(),
    )?;
    let work = item(
        &foreign,
        "foreign secret must not be read from foreign Home",
    )
    .await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let before = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("published Root absent")?;
    let view = get(&runtime, &work.id).await?;
    assert_eq!(view["work_item"]["agent_id"], "foreign-query");
    assert_eq!(view["work_item"]["plan_artifact_status"], "missing");
    assert!(view["work_item"]["plan_artifact"].is_null());
    assert!(std::path::Path::new(&work.plan_artifact.as_ref().unwrap().path).is_file());
    assert!(!crate::work_item_plan::plan_path(home.path(), &work.id).exists());
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    assert!(snapshot.selected_work_item(&work.id).is_none());
    let unavailable = snapshot.work.work_item(&work.id).unwrap_err();
    assert!(
        matches!(unavailable.downcast_ref::<crate::runtime::resident_work::WorkReadError>(),
        Some(crate::runtime::resident_work::WorkReadError::NeedsPreparation(key))
            if key.family == crate::runtime::resident_work::WorkFamily::Work && key.id == work.id),
        "foreign ordinary read must remain unqualified: {unavailable:#}"
    );
    assert!(snapshot.items.is_empty());
    assert!(Arc::ptr_eq(
        &before,
        &runtime
            .inner
            .mutation_owner
            .capture()?
            .context("published Root absent")?
    ));
    drop(snapshot);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    foreign.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn explicit_work_plan_inspection_rejects_oversized_file_before_returning_unreadable_view(
) -> Result<()> {
    let home = tempdir()?;
    let foreign_home = tempdir()?;
    let runtime = runtime(home.path())?;
    let foreign = RuntimeHandle::new_offline_with_runtime_db(
        "foreign-budget",
        foreign_home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        runtime.inner.runtime_db.clone(),
    )?;
    let work = item(&foreign, "foreign record, current Home inspection").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let path = crate::work_item_plan::plan_path(home.path(), &work.id);
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, "large current-home file\n".repeat(100_000))?;
    let mut snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let source = Arc::get_mut(snapshot.resource_source.as_mut().unwrap()).unwrap();
    let prior = *source.usage.lock().unwrap();
    let view = view_material(&work)?;
    let address = u64::try_from(work.id.len())?
        .checked_add(u64::try_from(
            runtime.inner.storage.data_dir().as_os_str().len(),
        )?)
        .and_then(|bytes| bytes.checked_add(8192))
        .and_then(|bytes| bytes.checked_mul(8))
        .context("actual inspection address cost overflow")?;
    // Admit the real eight-copy address first; the remaining 64KiB must reject
    // the oversized file, rather than failing before its actual metadata read.
    source.limits.0 = prior.0 + view.0 + address + 64 * 1024;
    assert!(std::fs::metadata(&path)?.len() * 8 > 64 * 1024);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .inspect_historical_work_item_plan(&snapshot, &work)
        .err()
        .context("oversized explicit file was returned as a diagnostic")?;
    assert!(
        matches!(
            error.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert_eq!(
        *snapshot
            .resource_source
            .as_ref()
            .unwrap()
            .usage
            .lock()
            .unwrap(),
        prior
    );
    assert!(snapshot.selected_work_item(&work.id).is_none());
    let unavailable = snapshot.work.work_item(&work.id).unwrap_err();
    assert!(
        matches!(unavailable.downcast_ref::<crate::runtime::resident_work::WorkReadError>(),
        Some(crate::runtime::resident_work::WorkReadError::NeedsPreparation(key))
            if key.family == crate::runtime::resident_work::WorkFamily::Work && key.id == work.id),
        "foreign ordinary read must remain unqualified: {unavailable:#}"
    );
    drop(snapshot);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    foreign.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn selected_closed_work_outside_display_window_is_pure_and_unselected_old_work_keeps_recall(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let mut records = Vec::new();
    for index in 0..130 {
        let mut record = WorkItemRecord::new(
            runtime.instance_agent_id(),
            format!("closed Work {index}"),
            crate::types::WorkItemState::Completed,
        );
        record.created_at =
            runtime.now() - chrono::Duration::days(30) + chrono::Duration::milliseconds(index);
        record.updated_at = record.created_at;
        // The original prepared Work INSERT validates the complete terminal
        // contract and commits its actual Brief in this same whole transaction.
        let mut brief = crate::types::BriefRecord::new(
            record.agent_id.clone(),
            crate::types::BriefKind::Result,
            format!("actual closed Work report {index}"),
            None,
            None,
        );
        brief.created_at = record.created_at;
        brief.work_item_id = Some(record.id.clone());
        brief.workspace_id = record.workspace_id.clone();
        record.result_brief_id = Some(brief.id.clone());
        record.completion_intent = Some(crate::types::WorkItemCompletionIntent {
            work_item_id: record.id.clone(),
            source_activation_id: None,
            source_message_id: None,
            source_turn_id: None,
            expected_work_revision: record.revision,
            report_requirement: crate::types::CompletionReportRequirement::Required,
            report_state: crate::types::CompletionReportState::Bound,
            result_brief_id: Some(brief.id.clone()),
            created_at: record.created_at,
            updated_at: record.updated_at,
        });
        let commit = {
            let mut owner = runtime.begin_mutation().await?;
            owner
                .commit_work_transition(
                    crate::ids::audit_event_id(),
                    crate::runtime_db::transitions::WorkItemTransitionCommand {
                        agent_id: record.agent_id.clone(),
                        mutation: crate::runtime_db::transitions::WorkItemMutation::Insert {
                            record: record.clone(),
                        },
                        agent_state: None,
                        brief_evidence: vec![brief],
                        audit_events: Vec::new(),
                        index_changes: Vec::new(),
                        notify_scheduler: true,
                        fault: None,
                    },
                    Default::default(),
                    true,
                )
                .await
                .with_context(|| format!("actual closed Work seed ACK index={index}"))?
        };
        runtime.apply_transition_commit(commit).await;
        records.push(record);
    }
    let target = &records[0];
    let unselected = &records[1];
    {
        let mut owner = runtime.begin_mutation().await?;
        let mut request = crate::runtime::resident_work::WorkCaptureRequest::current();
        request
            .mandatory_roots
            .insert(crate::runtime::resident_work::WorkKey::new(
                crate::runtime::resident_work::WorkFamily::Work,
                &target.id,
            ));
        owner.prepare_owner_work_request(&request).await?;
    }
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    assert_eq!(snapshot.items.len(), 128);
    assert!(!snapshot
        .items
        .iter()
        .any(|view| view.work_item.id == target.id));
    assert_eq!(snapshot.selected_work_item(&target.id), Some(target));
    assert!(snapshot.selected_work_item(&unselected.id).is_none());
    let root = runtime
        .inner
        .mutation_owner
        .capture()?
        .context("Root absent")?;
    let recalled = get(&runtime, &unselected.id).await?;
    assert_eq!(recalled["work_item"]["id"], unselected.id);
    assert_eq!(
        recalled["work_item"]["plan_artifact_status"],
        "not_recorded"
    );
    assert!(Arc::ptr_eq(
        &root,
        &runtime.inner.mutation_owner.capture()?.unwrap()
    ));
    // Only the selected body must survive removal of its ordinary SQL source.
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute_batch("DROP TABLE work_items")?;
        Ok(())
    })?;
    let selected = crate::diagnostics::resource_io::deny_readers(get(&runtime, &target.id)).await?;
    assert_eq!(selected["work_item"]["id"], target.id);
    assert_eq!(selected["work_item"]["state"], "completed");
    assert_eq!(
        selected["work_item"]["plan_artifact_status"],
        "not_recorded"
    );
    drop(snapshot);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn explicit_work_row_recall_gates_raw_and_native_before_decode_and_releases_failed_material(
) -> Result<()> {
    let home = tempdir()?;
    let foreign_home = tempdir()?;
    let runtime = runtime(home.path())?;
    let foreign = RuntimeHandle::new_offline_with_runtime_db(
        "foreign-row-budget",
        foreign_home.path().to_path_buf(),
        None::<crate::types::WorkspaceEntry>,
        runtime.inner.runtime_db.clone(),
    )?;
    let work = item(&foreign, "actual foreign row recall").await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let original = serde_json::to_string(&work)?;
    let mut dense = serde_json::to_value(&work)?;
    dense["work_refs"] = serde_json::json!([{
        "kind":"other","ref":"original-reference","reason":"native count before decode","status":"active",
        "last_seen_at":work.created_at,"metadata":{"many":vec![0;4096]},
    }]);
    for native in [false, true] {
        let payload = if native {
            serde_json::to_string(&dense)?
        } else {
            format!("{{{}", "x".repeat(65 * 1024))
        };
        runtime.inner.runtime_db.transaction(|tx| {
            tx.execute(
                "UPDATE work_items SET payload_json=?2 WHERE work_item_id=?1",
                rusqlite::params![&work.id, &payload],
            )?;
            Ok(())
        })?;
        let mut snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
        let source = Arc::get_mut(snapshot.resource_source.as_mut().unwrap()).unwrap();
        let prior = *source.usage.lock().unwrap();
        if native {
            source.limits.1 = prior.1 + 64;
        } else {
            source.limits.0 = prior.0 + 1024;
        }
        let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
        let error = runtime
            .inspect_historical_work_item(&snapshot, &work.id)
            .err()
            .context("over-budget row was decoded")?;
        assert!(
            matches!(
                error.downcast_ref::<crate::runtime_db::work_snapshot::WorkSnapshotError>(),
                Some(crate::runtime_db::work_snapshot::WorkSnapshotError::ResourceExhausted(_))
            ),
            "{error:#}"
        );
        assert!(!error.to_string().contains("malformed JSON"));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert_eq!(
            *snapshot
                .resource_source
                .as_ref()
                .unwrap()
                .usage
                .lock()
                .unwrap(),
            prior
        );
        assert!(snapshot.selected_work_item(&work.id).is_none());
    }
    runtime.inner.runtime_db.transaction(|tx| {
        tx.execute(
            "UPDATE work_items SET payload_json=?2 WHERE work_item_id=?1",
            rusqlite::params![&work.id, &original],
        )?;
        Ok(())
    })?;
    let snapshot = runtime.capture_work_query_snapshot_with_resources().await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let recall = runtime
        .inspect_historical_work_item(&snapshot, &work.id)?
        .context("real bounded recall absent")?;
    let (actual, material) = recall.into_parts();
    assert_eq!(actual, work);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    assert!(runtime
        .inspect_historical_work_item(&snapshot, "missing-explicit-work")?
        .is_none());
    drop(actual);
    drop(material);
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(snapshot.selected_work_item(&work.id).is_none());
    drop(snapshot);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    foreign.stop_context_resource_observer().await?;
    Ok(())
}
