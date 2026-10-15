//! Actual file kernel + original Work wholeACK; result wire needs no refresh.
use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::types::{AuthorityClass, WorkItemPlanStatus, WorkItemState};

fn runtime(home: &Path) -> Result<RuntimeHandle> {
    RuntimeHandle::new_isolated_resource_fixture(
        "created-result",
        home.to_path_buf(),
        context_config(),
    )
}
#[tokio::test]
async fn actual_create_result_uses_kernel_material_after_resource_close_through_tool_wire(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.stop_context_resource_observer().await?;
    // Resource refresh cannot become a post-ACK dependency of a successful
    // actual create. Core execution admission and its original SQL owner stay valid.
    let tool=crate::tool::tools::create_work_item::execute(&runtime,runtime.instance_agent_id(),
        &AuthorityClass::OperatorInstruction,&serde_json::json!({"objective":"actual create return","plan":"actual original file material"})).await?;
    let payload = tool.envelope.result.context("Create result absent")?;
    assert_eq!(
        payload["work_item"]["plan_artifact"]["preview"],
        "actual original file material"
    );
    let id = payload["work_item"]["id"]
        .as_str()
        .context("created ID absent")?;
    let canonical = runtime
        .latest_work_item(id)
        .await?
        .context("actual created row absent")?;
    assert_eq!(canonical.revision, 1);
    assert_eq!(
        canonical.plan_artifact.as_ref().unwrap().preview,
        "actual original file material"
    );
    assert!(matches!(
        runtime.inner.context_resources.freshness()?,
        crate::runtime::resource_observation::ResourceFreshness::Closed
    ));
    let owned = runtime
        .create_work_item_owned_result(
            "second actual result".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("retained original file".into()),
            Vec::new(),
        )
        .await?;
    let file = Arc::downgrade(&owned.material._file);
    let view_guard = Arc::downgrade(&owned.material._view);
    let id = owned.record.id.clone();
    std::fs::remove_file(crate::work_item_plan::plan_path(home.path(), &id))?;
    let (view, material) = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::work_item_query::view_for_created_work_item(&runtime, owned),
    )
    .await?;
    assert_eq!(
        view.plan_artifact.as_ref().unwrap().preview,
        "retained original file"
    );
    assert!(file.upgrade().is_some());
    assert!(view_guard.upgrade().is_some());
    let wire = crate::tool::tools::serialize_success(
        "CreateWorkItem",
        &crate::tool::tools::work_item_action::WorkItemMutationResult::new(view),
    )?;
    assert_eq!(
        wire.envelope.result.as_ref().unwrap()["work_item"]["plan_artifact_status"],
        "available"
    );
    assert!(file.upgrade().is_some());
    assert!(view_guard.upgrade().is_some());
    drop(material);
    assert!(file.upgrade().is_none());
    assert!(view_guard.upgrade().is_none());
    assert!(runtime.latest_work_item(&id).await?.is_some());
    Ok(())
}

#[tokio::test]
async fn create_file_outcome_and_owned_result_copies_share_cap_before_business_sql() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    runtime.stop_context_resource_observer().await?;
    let mut record = WorkItemRecord::new(
        runtime.instance_agent_id(),
        "original pre-SQL result allowance",
        WorkItemState::Open,
    );
    let (artifact, file, cost) = runtime
        .create_work_plan_file_with_material(&record, Some("original finite file source"))?;
    record.plan_artifact = Some(artifact);
    let full = runtime.prepare_created_work_item_result_material(&record, file.clone(), cost)?;
    let admitted = full.material_estimate();
    let view = (admitted.0 - cost.0, admitted.1 - cost.1);
    drop(full);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    for records in [false, true] {
        let limit = if records {
            (admitted.1 - 1, 64 * 1024 * 1024)
        } else {
            (65_536, admitted.0 - 1)
        };
        if records {
            assert!(cost.1 < limit.0);
            assert!(view.1 < limit.0);
        } else {
            assert!(cost.0 < limit.1);
            assert!(view.0 < limit.1);
        }
        let error = runtime
            .prepare_created_work_item_result_with_limits(&record, file.clone(), cost, limit)
            .err()
            .context("combined file/result cap admitted")?;
        assert!(matches!(
            error.downcast_ref::<ResourceObservationError>(),
            Some(ResourceObservationError::ResourceExhausted(_))
        ));
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        assert!(runtime.latest_work_item(&record.id).await?.is_none());
    }
    drop(file);
    Ok(())
}

#[tokio::test]
async fn actual_update_tool_hands_original_observed_plan_to_wire_without_file_reads() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "original update result".into(),
            None,
            Some("observed original update plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &original.id),
        "unobserved future plan",
    )?;
    let tool = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::update_work_item::execute(
            &runtime,
            runtime.instance_agent_id(),
            &AuthorityClass::OperatorInstruction,
            &serde_json::json!({"work_item_id":original.id,"objective":"actual updated result"}),
        ),
    )
    .await?;
    let payload = tool.envelope.result.context("Update result absent")?;
    assert_eq!(payload["work_item"]["objective"], "actual updated result");
    assert_eq!(
        payload["work_item"]["plan_artifact"]["preview"],
        "observed original update plan"
    );
    let actual = runtime.latest_work_item(&original.id).await?.unwrap();
    assert_eq!(actual.revision, original.revision + 1);
    assert_eq!(actual.plan_artifact, original.plan_artifact);
    let read = crate::diagnostics::resource_io::deny_readers(
        runtime.update_work_item_owned_result(original.id.clone(), None, None, None),
    )
    .await?;
    let source = Arc::downgrade(
        read.material
            ._original_read
            .as_ref()
            .context("original update observation absent")?,
    );
    let view_material = Arc::downgrade(&read.material._view);
    let (view, material) = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::work_item_query::view_for_owned_work_item(&runtime, read),
    )
    .await?;
    assert_eq!(
        view.plan_artifact.as_ref().unwrap().preview,
        "observed original update plan"
    );
    let wire = crate::tool::tools::serialize_success(
        "UpdateWorkItem",
        &crate::tool::tools::work_item_action::WorkItemMutationResult::new(view),
    )?;
    assert_eq!(
        wire.envelope.result.as_ref().unwrap()["work_item"]["plan_artifact_status"],
        "available"
    );
    assert!(source.upgrade().is_some());
    assert!(view_material.upgrade().is_some());
    drop(material);
    assert!(source.upgrade().is_none());
    assert!(view_material.upgrade().is_none());
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn noop_update_result_uses_observed_legacy_absence_without_starting_file_workflow(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "legacy noop result".into(),
            None,
            Some("remove original artifact".into()),
            Vec::new(),
        )
        .await?;
    let mut legacy = original.clone();
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
                        expected_revision: original.revision,
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
    let path = crate::work_item_plan::plan_path(home.path(), &legacy.id);
    std::fs::remove_file(&path)?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let tool = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::update_work_item::execute(
            &runtime,
            runtime.instance_agent_id(),
            &AuthorityClass::OperatorInstruction,
            &serde_json::json!({"work_item_id":legacy.id}),
        ),
    )
    .await?;
    let payload = tool.envelope.result.context("noop Update result absent")?;
    assert_eq!(payload["work_item"]["plan_artifact_status"], "not_recorded");
    assert!(payload["work_item"]["plan_artifact"].is_null());
    assert!(!path.exists());
    assert_eq!(runtime.latest_work_item(&legacy.id).await?.unwrap(), legacy);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn complete_tool_retains_original_plan_material_through_actual_prepared_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "actual Complete result".into(),
            None,
            Some("original observed completion plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &original.id),
        "unobserved replacement completion plan",
    )?;
    let candidate = crate::tool::spec::CompletionReportCandidate {
        text: "Actual original work is complete.".into(),
        citations: Vec::new(),
        source_turn_index: 1,
        source_round: 1,
        source_turn_id: None,
        source_message_id: None,
        source_assistant_round_id: "actual-completion-round".into(),
        source_tool_call_id: "actual-completion-tool".into(),
    };
    // Use the real Tool completion preparation/serialization function with its
    // existing explicit control authority; the original commit remains deferred.
    let mut tool = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::complete_work_item::complete_with_report_candidate(
            &runtime,
            original.id.clone(),
            crate::runtime::WorkItemCompletionAuthority::Control,
            Some(&candidate),
            Vec::new(),
            "same_assistant_round_preceding_text",
        ),
    )
    .await?;
    let payload = tool
        .envelope
        .result
        .as_ref()
        .context("Complete result absent")?;
    assert_eq!(payload["completion_phase"], "prepared");
    // The prepared Brief is not yet canonical. The original wire has no
    // existing summary/Legacy report, so it does not expose candidate text.
    assert!(payload["work_item"]["completion_report"].is_null());
    assert_eq!(payload["work_item"]["state"], "completed");
    assert_eq!(
        payload["work_item"]["plan_artifact"]["preview"],
        "original observed completion plan"
    );
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.unwrap(),
        original
    );
    let prepared = *tool
        .prepared_work_item_completion
        .take()
        .context("actual prepared completion absent")?;
    let material = prepared
        ._result_view_material
        .clone()
        .context("prepared completion result material absent")?;
    let source = Arc::downgrade(material._original_read.as_ref().unwrap());
    let view_guard = Arc::downgrade(&material._view);
    let actual_brief = prepared.brief.id.clone();
    assert!(source.upgrade().is_some());
    assert!(view_guard.upgrade().is_some());
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    let committed = crate::diagnostics::resource_io::deny_readers(
        runtime.commit_prepared_detached_work_item_completion(prepared),
    )
    .await?;
    let actual = runtime.latest_work_item(&original.id).await?.unwrap();
    assert_eq!(actual, committed.work_item);
    assert_eq!(actual.state, WorkItemState::Completed);
    assert_eq!(actual.plan_artifact, original.plan_artifact);
    assert_eq!(
        actual.result_brief_id.as_deref(),
        Some(actual_brief.as_str())
    );
    assert_eq!(
        runtime
            .recent_briefs(100)
            .await?
            .iter()
            .filter(|brief| brief.id == actual_brief)
            .count(),
        1
    );
    assert!(source.upgrade().is_some());
    assert!(view_guard.upgrade().is_some());
    drop(material);
    assert!(source.upgrade().is_none());
    assert!(view_guard.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn wait_tool_plan_material_survives_wire_and_actual_wait_whole_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "actual prepared Wait result".into(),
            None,
            Some("original observed wait plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    std::fs::write(
        crate::work_item_plan::plan_path(home.path(), &original.id),
        "unobserved replacement wait plan",
    )?;
    let mut tool = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::wait_for::prepare_settlement(
            &runtime,
            runtime.instance_agent_id(),
            &AuthorityClass::OperatorInstruction,
            crate::tool::tools::wait_for::WaitForArgs {
                reason: "actual original operator decision".into(),
                wake: crate::tool::tools::wait_for::WaitForWakeArg::OperatorInput,
                delivery: crate::tool::tools::wait_for::WaitForDeliveryArg::Silent,
                work_item_id: Some(original.id.clone()),
                resource: None,
                recheck_after_ms: None,
            },
        ),
    )
    .await?;
    assert!(tool.should_sleep);
    let payload = tool
        .envelope
        .result
        .as_ref()
        .context("Wait result absent")?;
    assert_eq!(payload["work_item"]["plan_artifact_status"], "available");
    assert_eq!(
        payload["work_item"]["plan_artifact"]["preview"],
        "original observed wait plan"
    );
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.unwrap(),
        original
    );
    let prepared = tool
        .prepared_wait_for
        .take()
        .context("actual prepared Wait absent")?;
    let material = prepared
        ._result_view_material
        .clone()
        .context("prepared Wait result material absent")?;
    let source = Arc::downgrade(material._original_read.as_ref().unwrap());
    let view_guard = Arc::downgrade(&material._view);
    let wait_id = prepared.registration.condition.id.clone();
    drop(pause);
    runtime.stop_context_resource_observer().await?;
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
    let actual = runtime.latest_work_item(&original.id).await?.unwrap();
    assert_eq!(actual.blocked_by_wait_id.as_deref(), Some(wait_id.as_str()));
    assert_eq!(actual.plan_artifact, original.plan_artifact);
    assert!(source.upgrade().is_some());
    assert!(view_guard.upgrade().is_some());
    drop(prepared);
    assert!(
        source.upgrade().is_some(),
        "wire loan still owns the original source"
    );
    drop(material);
    assert!(source.upgrade().is_none());
    assert!(view_guard.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn direct_wait_owned_result_binds_actual_ack_and_original_allocation_loan() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "direct original Wait".into(),
            None,
            Some("direct observed plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let original_target = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .work
        .as_ref()
        .unwrap()
        .qualified_context_plan_source(&original.id)
        .unwrap()
        .clone();
    let original_read = runtime.capture_observed_work_plan(&original.id, original.revision)?;
    let borrowed_material =
        runtime.prepare_observed_work_item_result_material(&original, original_read, false)?;
    assert!(borrowed_material.shares_work_allocation(&original_target));
    let equal_but_distinct = Arc::new(original_target.as_ref().clone());
    assert!(!borrowed_material.shares_work_allocation(&equal_but_distinct));
    drop(borrowed_material);
    let (outcome, read) = crate::diagnostics::resource_io::deny_readers(
        runtime.register_wait_for_outcome_owned_result(
            runtime.instance_agent_id(),
            Some(original.id.clone()),
            crate::runtime::WaitForWakeKind::OperatorInput,
            None,
            "direct original wait reason".into(),
            None,
        ),
    )
    .await?;
    let crate::runtime::WaitForRegistrationOutcome::Registered { registration } = outcome else {
        anyhow::bail!("direct actual Wait did not register")
    };
    let read = read.context("direct actual Wait result material absent")?;
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.unwrap(),
        read.record
    );
    assert_eq!(registration.work_item.as_ref(), Some(&read.record));
    assert!(read.material.shares_work_allocation(&original_target));
    let current_target = runtime
        .inner
        .mutation_owner
        .capture()?
        .unwrap()
        .work
        .as_ref()
        .unwrap()
        .qualified_context_plan_source(&original.id)
        .unwrap()
        .clone();
    assert!(!read.material.shares_work_allocation(&current_target));
    let source = Arc::downgrade(read.material._original_read.as_ref().unwrap());
    let view_guard = Arc::downgrade(&read.material._view);
    let (view, material) = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::work_item_query::view_for_owned_work_item(&runtime, read),
    )
    .await?;
    let wire = crate::tool::tools::serialize_success(
        "WaitFor",
        &crate::tool::tools::work_item_action::WorkItemMutationResult::new(view),
    )?;
    assert_eq!(
        wire.envelope.result.as_ref().unwrap()["work_item"]["plan_artifact_status"],
        "available"
    );
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    assert!(source.upgrade().is_some());
    assert!(view_guard.upgrade().is_some());
    drop(material);
    assert!(source.upgrade().is_none());
    assert!(view_guard.upgrade().is_none());
    // A newly created result has no previous owning qualified allocation to loan.
    let created = runtime
        .create_work_item_owned_result(
            "Create does not deduplicate by body".into(),
            None,
            Some("fresh file".into()),
            Vec::new(),
        )
        .await?;
    let target = created.material._target.as_ref().unwrap();
    assert!(!created.material.shares_work_allocation(target));
    Ok(())
}

#[tokio::test]
async fn planned_work_result_rejects_a_forged_file_outcome_before_result_clone() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "strict planned outcome".into(),
            None,
            Some("original file".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let observed = runtime.capture_observed_work_plan(&original.id, original.revision)?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let mut forged = original.clone();
    forged.plan_artifact.as_mut().unwrap().preview = "forged result body".into();
    let error = runtime
        .prepare_planned_work_item_result_read(&forged, observed.clone())
        .err()
        .context("forged original plan admitted")?;
    assert!(error.to_string().contains("original plan outcome"));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.unwrap(),
        original
    );
    drop(observed);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn successful_writer_result_never_uses_successor_body_wait_projection() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let read = runtime
        .create_work_item_owned_result(
            "original successful Create record".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("original successful file outcome".into()),
            Vec::new(),
        )
        .await?;
    let original = read.record.clone();
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let registration = runtime
        .register_wait_for(
            runtime.instance_agent_id(),
            Some(original.id.clone()),
            crate::runtime::WaitForWakeKind::OperatorInput,
            None,
            "successor Wait body only".into(),
            None,
        )
        .await?;
    let snapshot = runtime.capture_work_query_snapshot().await?;
    let successor = snapshot
        .items
        .iter()
        .find(|item| item.work_item.id == original.id)
        .context("successor selected projection absent")?;
    assert_ne!(successor.work_item, original);
    assert!(successor.has_active_waits);
    assert_eq!(
        successor.work_item.blocked_by_wait_id.as_deref(),
        Some(registration.condition.id.as_str())
    );
    let context = crate::tool::tools::work_item_query::query_context_from_work_items(
        &snapshot.state,
        &snapshot.items,
    );
    let expected =
        crate::tool::tools::work_item_query::projection_outside_query_window(&context, &original);
    let (view, material) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    assert_eq!(view.objective, original.objective);
    assert!(view.blocked_by.is_none());
    assert!(view.active_wait_conditions.is_empty());
    assert_eq!(view.readiness, expected.readiness);
    assert_eq!(view.reason_code, expected.reason_code);
    assert_eq!(view.plan_artifact, original.plan_artifact);
    let wire = crate::tool::tools::serialize_success(
        "CreateWorkItem",
        &crate::tool::tools::work_item_action::WorkItemMutationResult::new(view),
    )?;
    assert_eq!(
        wire.envelope.result.as_ref().unwrap()["work_item"]["objective"],
        original.objective
    );
    assert!(runtime
        .latest_work_item(&original.id)
        .await?
        .unwrap()
        .blocked_by_wait_id
        .is_some());
    drop(material);
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn result_projection_context_comes_from_its_single_held_snapshot() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "first selected focus".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("first focus plan".into()),
            Vec::new(),
        )
        .await?;
    let successor = runtime
        .create_work_item(
            "second selected focus".into(),
            Some(WorkItemPlanStatus::Ready),
            Some("second focus plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.pick_work_item(original.id.clone()).await?;
    runtime.refresh_context_resources().await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let read = runtime
        .update_work_item_owned_result(original.id.clone(), None, None, None)
        .await?;
    let view_guard = Arc::downgrade(&read.material._view);
    let snapshot = runtime.capture_work_query_snapshot().await?;
    assert_eq!(
        snapshot.state.current_work_item_id.as_deref(),
        Some(original.id.as_str())
    );
    runtime.pick_work_item(successor.id.clone()).await?;
    assert_eq!(
        runtime.agent_state().await?.current_work_item_id.as_deref(),
        Some(successor.id.as_str())
    );
    let (view, material) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    assert!(
        view.is_current,
        "later publication must not replace the selected view's State"
    );
    assert_eq!(view.plan_artifact, original.plan_artifact);
    let wire = crate::tool::tools::serialize_success(
        "UpdateWorkItem",
        &crate::tool::tools::work_item_action::WorkItemMutationResult::new(view),
    )?;
    assert_eq!(
        wire.envelope.result.as_ref().unwrap()["work_item"]["is_current"],
        true
    );
    assert!(view_guard.upgrade().is_some());
    drop(material);
    assert!(view_guard.upgrade().is_none());
    drop(pause);
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn prepared_complete_report_is_frozen_before_sql_and_never_queries_unwritten_brief_or_legacy_sources(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let original = runtime
        .create_work_item(
            "original prepared report".into(),
            None,
            Some("original report plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let dispatch = runtime
        .prepare_work_item_completion_with_report(
            original.id.clone(),
            crate::runtime::WorkItemCompletionAuthority::Control,
            "original prepared completion text".into(),
            Vec::new(),
            Some(7),
            None,
            None,
            None,
            None,
            None,
            "control",
            Vec::new(),
        )
        .await?;
    let crate::runtime::WorkItemCompletionDispatch::Prepared(mut prepared) = dispatch else {
        anyhow::bail!("actual prepared completion absent")
    };
    let original_nonce_brief = prepared.brief.id.clone();
    assert!(runtime
        .storage()
        .read_brief_by_id(&original_nonce_brief)?
        .is_none());
    let read = runtime.prepare_completion_result_read(&mut prepared)?;
    runtime.stop_context_resource_observer().await?;
    let material = read.material().clone();
    let weak = Arc::downgrade(&material);
    let proof = Arc::downgrade(material._prepared_report_material.as_ref().unwrap());
    assert!(material.prepared_report(&prepared.record)?.is_some());
    let mut forged = prepared.record.clone();
    forged.result_summary = Some("successor report must not replace frozen output".into());
    assert!(material.prepared_report(&forged).is_err());
    forged = prepared.record.clone();
    forged.result_brief_id = Some("another-source".into());
    assert!(material.prepared_report(&forged).is_err());
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("DROP TABLE briefs; DROP TABLE delivery_summaries;")?;
    let (view, wire) = crate::diagnostics::resource_io::deny_readers(
        crate::tool::tools::work_item_query::view_for_owned_work_item(&runtime, read),
    )
    .await?;
    let json = serde_json::to_value(&view)?;
    assert!(
        json["completion_report"].is_null(),
        "the unwritten prepared Brief must not become an accepted report source"
    );
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.unwrap(),
        original,
        "planned result must not write canonical Work"
    );
    drop(prepared);
    drop(material);
    assert!(proof.upgrade().is_some());
    drop(wire);
    assert!(weak.upgrade().is_none());
    assert!(proof.upgrade().is_none());
    Ok(())
}

async fn original_report_completion(
    runtime: &RuntimeHandle,
) -> Result<(WorkItemRecord, crate::tool::ToolResult)> {
    let original = runtime
        .create_work_item(
            "original successful report".into(),
            None,
            Some("original report plan".into()),
            Vec::new(),
        )
        .await?;
    runtime.refresh_context_resources().await?;
    let candidate = crate::tool::spec::CompletionReportCandidate {
        text: "original successfully accepted report".into(),
        citations: Vec::new(),
        source_turn_index: 1,
        source_round: 1,
        source_turn_id: None,
        source_message_id: None,
        source_assistant_round_id: "original-source-round".into(),
        source_tool_call_id: "original-source-tool".into(),
    };
    let result = crate::tool::tools::complete_work_item::complete_with_report_candidate(
        runtime,
        original.id.clone(),
        crate::runtime::WorkItemCompletionAuthority::Control,
        Some(&candidate),
        Vec::new(),
        "same_assistant_round_preceding_text",
    )
    .await?;
    Ok((original, result))
}

#[tokio::test]
async fn acknowledged_original_report_allows_only_exact_prepared_whole_receipt_replay() -> Result<()>
{
    use crate::runtime_db::transitions::{
        AgentStateMutation, PreparedInputTransition, TransitionFaultPoint,
        WorkItemFocusTransitionCommand, WorkItemMutation,
    };
    for fault in [None, Some(TransitionFaultPoint::BeforeCacheUpdate)] {
        let home = tempdir()?;
        let runtime = runtime(home.path())?;
        let (_original, mut tool) = original_report_completion(&runtime).await?;
        let planned = *tool.prepared_work_item_completion.take().unwrap();
        let material = planned._result_view_material.clone().unwrap();
        let actual_record = planned.record.clone();
        let scope = material.scope.clone();
        let frozen_at = chrono::Utc::now();
        let nonce = crate::ids::audit_event_id();
        let build = |nonce: String,
                     record: crate::types::WorkItemRecord|
         -> Result<PreparedInputTransition> {
            let command = WorkItemFocusTransitionCommand {
                agent_id: record.agent_id.clone(),
                work_items: vec![WorkItemMutation::Update {
                    expected_revision: record.revision - 1,
                    record,
                }],
                wait_conditions: planned.wait_conditions.clone(),
                active_continuation_expectations: None,
                continuation_expectations: planned.continuation_expectations.clone(),
                continuations: planned.continuations.clone(),
                agent_state: AgentStateMutation {
                    expected: Some(Box::new(planned.expected_agent_state.clone())),
                    record: Box::new(planned.committed_agent_state.clone()),
                },
                brief_evidence: vec![planned.brief.clone()],
                audit_events: planned.audit_events.clone(),
                index_changes: planned.index_changes.clone(),
                notify_scheduler: true,
                fault,
            };
            PreparedInputTransition::focus(
                scope.clone(),
                nonce,
                frozen_at,
                command,
                Default::default(),
                true,
                planned.tool_execution.clone(),
            )?
            .with_work_resource_material(
                crate::runtime::WorkResourceMaterialLoan::result(material.clone()),
            )
        };
        // This is one actual Tool-planned completion and one frozen D3 nonce,
        // unlike the outer detached convenience method which allocates a nonce.
        let prepared = build(nonce.clone(), actual_record.clone())?;
        let replay_prepared = build(nonce.clone(), actual_record.clone())?;
        assert_eq!(prepared.descriptor(), replay_prepared.descriptor());
        let original_attempt = prepared_report_digest(prepared.descriptor())?;
        assert_eq!(
            original_attempt,
            prepared_report_digest(replay_prepared.descriptor())?
        );
        let mut owner = runtime.begin_mutation().await?;
        let first = owner.commit_prepared_input_transition(prepared).await;
        if fault.is_some() {
            let error = first
                .err()
                .context("actual publication fault was not exercised")?;
            assert!(
                format!("{error:#}").contains("publication requires reconciliation"),
                "{error:#}"
            );
        } else {
            assert!(first?.applied);
        }
        let frozen = material
            .acknowledged_report
            .get()
            .context("real ACK omitted original Source")?
            .clone();
        assert_eq!(frozen.attempt, original_attempt);
        assert!(material.pending_report.lock().unwrap().is_none());
        drop(owner);
        let mut owner = runtime.begin_mutation().await?;
        let counts = || -> Result<(usize, usize, usize, u64)> {
            Ok(runtime.inner.runtime_db.connection()?.query_row(
                "SELECT (SELECT COUNT(*) FROM input_mutation_receipts WHERE command_nonce=?1),
                 (SELECT COUNT(*) FROM briefs WHERE evidence_id=?2),
                 (SELECT COUNT(*) FROM audit_events WHERE agent_id=?3),
                 (SELECT revision FROM work_items WHERE work_item_id=?4)",
                rusqlite::params![nonce, planned.brief.id, scope.agent_id, actual_record.id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )?)
        };
        let before = counts()?;
        assert_eq!(before.0, 1);
        assert_eq!(before.1, 1);
        let replay = owner
            .commit_prepared_input_transition(replay_prepared)
            .await?;
        assert!(replay.applied);
        assert_eq!(counts()?, before);
        assert!(Arc::ptr_eq(
            material.acknowledged_report.get().unwrap(),
            &frozen
        ));
        assert!(material.pending_report.lock().unwrap().is_none());
        assert_eq!(
            runtime
                .inner
                .runtime_db
                .work_items()
                .latest(&actual_record.id)?,
            Some(actual_record.clone())
        );
        // A different nonce or full record must not borrow the acknowledged
        // source to create a fresh attempt. Neither reaches business SQL.
        let foreign_nonce = build(crate::ids::audit_event_id(), actual_record.clone())?;
        assert!(owner
            .commit_prepared_input_transition(foreign_nonce)
            .await
            .is_err());
        let mut foreign_record = actual_record.clone();
        foreign_record
            .objective
            .push_str(" changed original record");
        let foreign_record = build(nonce.clone(), foreign_record)?;
        assert!(owner
            .commit_prepared_input_transition(foreign_record)
            .await
            .is_err());
        assert_eq!(counts()?, before);
        assert!(Arc::ptr_eq(
            material.acknowledged_report.get().unwrap(),
            &frozen
        ));
        assert!(material.pending_report.lock().unwrap().is_none());
        drop(owner);
        let weak = Arc::downgrade(&frozen);
        drop(frozen);
        drop(planned);
        drop(material);
        drop(tool);
        assert!(weak.upgrade().is_none());
    }
    Ok(())
}

#[tokio::test]
async fn actual_completion_ack_carries_original_qualified_report_through_successor_damage_and_pure_wire(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool
        .prepared_work_item_completion
        .take()
        .context("original prepared completion absent")?;
    let expected = prepared.brief.clone();
    let material = prepared._result_view_material.clone().unwrap();
    assert!(material.acknowledged_report.get().is_none());
    let actual = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await?
        .work_item;
    assert_eq!(actual.state, WorkItemState::Completed);
    assert_eq!(actual.plan_artifact, original.plan_artifact);
    let report = material
        .acknowledged_report
        .get()
        .context("actual wholeACK omitted original report")?;
    assert!(report.total_cost.0 > material.cost.0 && report.total_cost.1 > material.cost.1);
    let projection = report
        .projection
        .as_ref()
        .context("actual accepted report empty")?;
    assert_eq!(projection.text, expected.text);
    assert_eq!(projection.brief_id.as_deref(), Some(expected.id.as_str()));
    let source = report
        ._sources
        .records_for_ack_test()
        .iter()
        .find(|body| {
            body.locator.key.family
                == crate::runtime::resident_input_evidence::InputEvidenceFamily::Brief
        })
        .context("report did not retain its original qualified Brief")?;
    let source_weak = Arc::downgrade(source);
    let report_weak = Arc::downgrade(&report._guards[0]);
    let read = OwnedWorkItemMutationRead {
        record: actual,
        material: material.clone(),
    };
    let snapshot = runtime
        .capture_work_query_snapshot_for_mutation_result(read.material())
        .await?;
    // Adversarial later storage damage is not a new source for the original
    // successful result. This does not claim a sanctioned immutable Brief update.
    runtime.inner.runtime_db.connection()?.execute(
        "UPDATE briefs SET payload_json='{' WHERE evidence_id=?1",
        [&expected.id],
    )?;
    runtime.stop_context_resource_observer().await?;
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TABLE briefs;DROP TABLE delivery_summaries;DROP TABLE transcript_entries;DROP TABLE messages;DROP TABLE work_items;")?;
    let (view, wire) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    let report = view
        .completion_report
        .as_ref()
        .context("original accepted view report absent")?;
    assert_eq!(report.text, expected.text);
    assert_eq!(report.brief_id.as_deref(), Some(expected.id.as_str()));
    let result = crate::tool::tools::serialize_success(
        "CompleteWorkItem",
        &crate::tool::tools::work_item_action::WorkItemMutationResult::new(view),
    )?;
    assert_eq!(
        result.envelope.result.as_ref().unwrap()["work_item"]["completion_report"]["text"],
        expected.text
    );
    drop(material);
    drop(tool);
    drop(runtime);
    assert!(report_weak.upgrade().is_some());
    assert!(source_weak.upgrade().is_some());
    drop(wire);
    assert!(report_weak.upgrade().is_none());
    assert!(source_weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn report_before_commit_rollback_does_not_install_once_source_then_real_retry_can_ack(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool.prepared_work_item_completion.take().unwrap();
    let material = prepared._result_view_material.clone().unwrap();
    let brief_id = prepared.brief.id.clone();
    // The original read is already frozen. Remove background refresh from the
    // accounting comparison without relaxing the actual business attempt.
    runtime.stop_context_resource_observer().await?;
    let before = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::BeforeCommit,
    );
    let error = runtime
        .commit_prepared_detached_work_item_completion(prepared.clone())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("BeforeCommit"), "{error:#}");
    assert!(material.acknowledged_report.get().is_none());
    assert!(material.pending_report.lock().unwrap().is_none());
    assert_eq!(
        runtime.latest_work_item(&original.id).await?.as_ref(),
        Some(&original)
    );
    assert!(runtime.storage().read_brief_by_id(&brief_id)?.is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        before
    );
    let actual = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await?
        .work_item;
    assert!(material.acknowledged_report(&actual)?.flatten().is_some());
    let count: u64 = runtime.inner.runtime_db.connection()?.query_row(
        "SELECT COUNT(*) FROM briefs WHERE evidence_id=?1",
        [&brief_id],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    Ok(())
}

#[tokio::test]
async fn actual_report_source_survives_known_publication_fault_and_original_owner_recovery(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (_original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool.prepared_work_item_completion.take().unwrap();
    let record = prepared.record.clone();
    let brief = prepared.brief.clone();
    let material = prepared._result_view_material.clone().unwrap();
    runtime.inject_next_transition_fault(
        crate::runtime_db::transitions::TransitionFaultPoint::BeforeCacheUpdate,
    );
    let error = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("publication requires reconciliation"),
        "{error:#}"
    );
    let frozen = material
        .acknowledged_report
        .get()
        .context("durable report was lost on publication error")?
        .clone();
    assert_eq!(frozen.projection.as_ref().unwrap().text, brief.text);
    let owner = runtime.begin_mutation().await?;
    assert!(owner
        .capture_published_root()?
        .work
        .as_ref()
        .unwrap()
        .qualified_context_plan_source(&record.id)
        .is_some());
    drop(owner);
    assert!(Arc::ptr_eq(
        material.acknowledged_report.get().unwrap(),
        &frozen
    ));
    let read = OwnedWorkItemMutationRead { record, material };
    let snapshot = runtime
        .capture_work_query_snapshot_for_mutation_result(read.material())
        .await?;
    runtime.stop_context_resource_observer().await?;
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("DROP TABLE briefs;DROP TABLE delivery_summaries;DROP TABLE work_items;")?;
    let (view, _wire) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    assert_eq!(view.completion_report.unwrap().text, brief.text);
    Ok(())
}

#[tokio::test]
async fn unchanged_completed_result_uses_one_actual_owner_source_without_later_report_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (_original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool.prepared_work_item_completion.take().unwrap();
    let brief = prepared.brief.clone();
    let actual = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await?
        .work_item;
    let read = runtime
        .capture_unchanged_work_item_result_read(&actual)
        .await?;
    assert_eq!(
        read.material
            .acknowledged_report(&actual)?
            .flatten()
            .unwrap()
            .text,
        brief.text
    );
    let snapshot = runtime
        .capture_work_query_snapshot_for_mutation_result(read.material())
        .await?;
    runtime.stop_context_resource_observer().await?;
    runtime.inner.runtime_db.connection()?.execute_batch("DROP TABLE briefs;DROP TABLE delivery_summaries;DROP TABLE transcript_entries;DROP TABLE messages;DROP TABLE work_items;")?;
    let (view, _wire) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    let report = view.completion_report.unwrap();
    assert_eq!(report.text, brief.text);
    assert_eq!(report.brief_id.as_deref(), Some(brief.id.as_str()));
    Ok(())
}

#[tokio::test]
async fn original_report_commit_unknown_keeps_same_attempt_source_until_whole_receipt() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (_original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool.prepared_work_item_completion.take().unwrap();
    let record = prepared.record.clone();
    let expected = prepared.brief.clone();
    let material = prepared._result_view_material.clone().unwrap();
    material
        .lose_report_ack
        .store(true, std::sync::atomic::Ordering::Release);
    let error = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("lost original Work report COMMIT"),
        "{error:#}"
    );
    assert!(material.acknowledged_report.get().is_none());
    let pending = material
        .pending_report
        .lock()
        .unwrap()
        .clone()
        .context("unknown lost original attempt")?;
    let frozen = pending
        .borrowed_source()
        .context("unknown lost original Source")?;
    assert_eq!(frozen.projection.as_ref().unwrap().text, expected.text);
    let receipt = runtime
        .inner
        .runtime_db
        .transaction(|tx| {
            crate::runtime_db::input_mutation_receipts::lookup_input_mutation_receipt_tx(
                tx,
                pending.original.prepared(),
            )
        })?
        .context("actual lost ACK has no whole receipt")?;
    let foreign = crate::runtime_db::input_mutation_receipts::OriginalInputMutationAttempt::new(
        receipt.prepared.clone(),
    );
    assert!(pending.confirm_receipt(&foreign, &receipt).is_err());
    assert!(material.acknowledged_report.get().is_none());
    assert!(Arc::ptr_eq(&pending.borrowed_source().unwrap(), &frozen));
    let owner = runtime.begin_mutation().await?;
    drop(owner);
    assert!(material.pending_report.lock().unwrap().is_none());
    assert!(Arc::ptr_eq(
        material.acknowledged_report.get().unwrap(),
        &frozen
    ));
    assert_eq!(
        runtime.latest_work_item(&record.id).await?.as_ref(),
        Some(&record)
    );
    let read = OwnedWorkItemMutationRead {
        record,
        material: material.clone(),
    };
    let snapshot = runtime
        .capture_work_query_snapshot_for_mutation_result(read.material())
        .await?;
    runtime.stop_context_resource_observer().await?;
    runtime
        .inner
        .runtime_db
        .connection()?
        .execute_batch("DROP TABLE briefs;DROP TABLE delivery_summaries;DROP TABLE work_items;")?;
    let (view, wire) = crate::diagnostics::resource_io::deny_readers(async {
        crate::tool::tools::work_item_query::view_for_owned_work_item_in_snapshot(
            &runtime, snapshot, read,
        )
    })
    .await?;
    assert_eq!(view.completion_report.as_ref().unwrap().text, expected.text);
    let weak = Arc::downgrade(&frozen);
    drop(frozen);
    drop(pending);
    drop(material);
    drop(tool);
    drop(runtime);
    assert!(weak.upgrade().is_some());
    drop(wire);
    assert!(weak.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn unknown_owner_witness_retains_source_after_original_result_consumer_is_dropped(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (_original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool.prepared_work_item_completion.take().unwrap();
    let material = prepared._result_view_material.clone().unwrap();
    material
        .lose_report_ack
        .store(true, std::sync::atomic::Ordering::Release);
    let error = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("lost original Work report COMMIT"),
        "{error:#}"
    );
    let pending = material.pending_report.lock().unwrap().clone().unwrap();
    let source = pending.borrowed_source().unwrap();
    let weak = Arc::downgrade(&source);
    let source_only = (
        usize::try_from(source.retained_cost.0)?,
        source.retained_cost.1,
    );
    let scope = material.scope.clone();
    drop(source);
    drop(pending);
    drop(material);
    drop(tool);
    assert!(
        weak.upgrade().is_some(),
        "original Unknown witness must own its frozen Source"
    );
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let recovery = reservation.recovery()?.unwrap();
    let retained = recovery
        .witness
        .as_ref()
        .unwrap()
        .original_work_report_material_prior(&scope, &runtime.inner.input_evidence_budget)?;
    assert_eq!(
        retained, source_only,
        "dropped original plan/result allocations are not retained Source prior"
    );
    drop(recovery);
    drop(reservation);
    let owner = runtime.begin_mutation().await?;
    drop(owner);
    assert!(
        weak.upgrade().is_none(),
        "receipt resolution releases a Source with no remaining consumer"
    );
    runtime.stop_context_resource_observer().await?;
    Ok(())
}

#[tokio::test]
async fn original_report_witness_prior_and_actual_input_material_share_one_remaining_cap(
) -> Result<()> {
    use crate::runtime_db::{
        input_evidence_snapshot::{InputEvidenceLoadRequest, InputEvidenceSnapshotError},
        probe_original_report_input_prior_for_test as probe,
    };
    let home = tempdir()?;
    let runtime = runtime(home.path())?;
    let (_original, mut tool) = original_report_completion(&runtime).await?;
    let prepared = *tool.prepared_work_item_completion.take().unwrap();
    let material = prepared._result_view_material.clone().unwrap();
    runtime.stop_context_resource_observer().await?;
    material
        .lose_report_ack
        .store(true, std::sync::atomic::Ordering::Release);
    let error = runtime
        .commit_prepared_detached_work_item_completion(prepared)
        .await
        .unwrap_err();
    assert!(
        format!("{error:#}").contains("lost original Work report COMMIT"),
        "{error:#}"
    );
    let reservation = runtime.inner.mutation_owner.reserve().await?;
    let recovery = reservation
        .recovery()?
        .context("actual report attempt is not Unknown")?;
    let witness = recovery
        .witness
        .as_ref()
        .context("actual Unknown has no original witness")?;
    let scope = material.scope.clone();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let prior = witness.original_work_report_material_prior(&scope, ledger)?;
    assert_eq!(
        prior,
        (
            usize::try_from(material.material_estimate().0)?,
            material.material_estimate().1
        )
    );
    assert!(prior.0 > 0 && prior.1 > 0);
    let foreign_scope = InputEvidenceScope {
        agent_id: "foreign-report-scope".into(),
        identity_incarnation: scope.identity_incarnation,
    };
    assert!(witness
        .original_work_report_material_prior(&foreign_scope, ledger)
        .is_err());
    let other = InputEvidenceBudgetLedger::new(ledger.limits())?;
    assert!(witness
        .original_work_report_material_prior(&scope, &other)
        .is_err());
    assert_eq!(other.report()?.observed_bytes, 0);
    assert_eq!(
        ledger.report()?.observed_bytes,
        baseline,
        "borrowed getter must not register old allocations"
    );
    let mut request = InputEvidenceLoadRequest {
        windows: Vec::new(),
        mandatory_roots: Default::default(),
        interrupted_sources: Default::default(),
        selected_completion_brief_ids: Default::default(),
        record_limit: 65_536,
        byte_limit: 64 * 1024 * 1024,
    };
    let projected = (32_768, 64);
    let incoming = probe(&scope, &request, ledger, (0, 0), projected)?;
    let total = (
        prior.0.checked_add(incoming.0).unwrap(),
        prior.1.checked_add(incoming.1).unwrap(),
    );
    assert!(total.0 <= request.byte_limit && total.1 <= request.record_limit);
    request.byte_limit = total.0 - 1;
    assert!(prior.0 <= request.byte_limit && incoming.0 <= request.byte_limit);
    let error = probe(&scope, &request, ledger, prior, projected).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    request.byte_limit = total.0;
    request.record_limit = total.1 - 1;
    assert!(prior.1 <= request.record_limit && incoming.1 <= request.record_limit);
    let error = probe(&scope, &request, ledger, prior, projected).unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<InputEvidenceSnapshotError>(),
            Some(InputEvidenceSnapshotError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    request.record_limit = total.1;
    assert_eq!(probe(&scope, &request, ledger, prior, projected)?, total);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    assert!(material.acknowledged_report.get().is_none());
    drop(recovery);
    drop(reservation);
    let owner = runtime.begin_mutation().await?;
    drop(owner);
    assert!(material.acknowledged_report.get().is_some());
    Ok(())
}
