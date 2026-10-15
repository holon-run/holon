use super::*;
use crate::runtime::tests::support::{context_config, tempdir};
use crate::types::{BriefKind, BriefRecord};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "child-observation-source",
        home.path().into(),
        context_config(),
    )?;
    {
        let mut mutation = runtime.begin_mutation().await?;
        mutation
            .state
            .working_memory
            .current_working_memory
            .work_summary = Some("actual selected work summary".repeat(256));
        mutation.persist_state(runtime.storage()).await?;
    }
    for (kind, text) in [
        (BriefKind::Ack, "original progress"),
        (BriefKind::Result, "original result"),
    ] {
        let brief = BriefRecord::new(
            runtime.instance_agent_id(),
            kind,
            text.repeat(1024),
            None,
            None,
        );
        let event = crate::types::brief_created_event_for(&brief)?;
        let mut mutation = runtime.begin_mutation().await?;
        let prepared =
            crate::runtime_db::prepared_input_evidence::PreparedInputEvidence::brief_created(
                mutation.input_evidence_scope(),
                event.id.clone(),
                brief.created_at,
                brief,
                event,
                Vec::new(),
            )?;
        mutation.commit_prepared_evidence(prepared, None).await?;
    }
    // Use the original named preparation, not a hand-built Ready/Source view.
    {
        let mut mutation = runtime.begin_mutation().await?;
        drop(
            mutation
                .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
                .await?,
        );
    }
    runtime.refresh_context_resources().await?;
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    Ok((home, runtime))
}
fn deny(runtime: &RuntimeHandle) -> Result<()> {
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO child_read_forbidden_state;
         ALTER TABLE agent_identities RENAME TO child_read_forbidden_identity;
         ALTER TABLE tasks RENAME TO child_read_forbidden_tasks;
         ALTER TABLE work_items RENAME TO child_read_forbidden_work;
         ALTER TABLE wait_conditions RENAME TO child_read_forbidden_waits;
         ALTER TABLE briefs RENAME TO child_read_forbidden_briefs;
         ALTER TABLE messages RENAME TO child_read_forbidden_messages;
         ALTER TABLE queue_entries RENAME TO child_read_forbidden_queue;
         ALTER TABLE audit_events RENAME TO child_read_forbidden_events;",
    )?;
    Ok(())
}
fn exhausted(error: &anyhow::Error) -> bool {
    use crate::runtime::{
        current_read_facts::CurrentReadError, resident_input_evidence::InputCaptureError,
        resident_scheduling::SchedulingReadError, resident_task::TaskReadError,
        resident_work::WorkReadError,
    };
    matches!(
        error.downcast_ref::<ChildAgentSummaryReadError>(),
        Some(ChildAgentSummaryReadError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<InputCaptureError>(),
        Some(InputCaptureError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<TaskReadError>(),
        Some(TaskReadError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<WorkReadError>(),
        Some(WorkReadError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::ResourceExhausted(_))
    )
}
#[tokio::test]
async fn original_loaded_child_observation_keeps_exact_selected_source_and_dto_without_sql_or_root_backedge(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let original = runtime.capture_child_observability_projection().await?;
    let refs = original.projection.active_tasks.iter().collect::<Vec<_>>();
    let briefs = original
        .observability_briefs
        .iter()
        .filter_map(|record| match &record.record {
            super::super::resident_input_evidence::PreparedInputEvidence::Brief(brief) => {
                Some(brief)
            }
            _ => None,
        });
    let closure = RuntimeHandle::closure_decision_from_projection(
        &original.state,
        &original.projection,
        None,
    );
    let expected =
        super::super::lifecycle::build_child_agent_observability_with_active_tasks_observed(
            &original.state,
            closure.waiting_reason,
            &refs,
            briefs,
            &mut None,
        )?;
    assert!(expected
        .last_progress_brief
        .as_ref()
        .is_some_and(|text| text.starts_with("original progress")));
    assert!(expected
        .last_result_brief
        .as_ref()
        .is_some_and(|text| text.starts_with("original result")));
    drop(refs);
    drop(original);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let strong = Arc::strong_count(&root);
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let expected_identity = root.active_identity()?;
    deny(&runtime)?;
    let read = runtime
        .capture_owned_child_summary_with_material(expected_identity, LIMITS, (0, 0))
        .await?;
    assert_eq!(read.summary.observability, expected);
    assert_eq!(
        read.summary.identity,
        AgentIdentityView::from_record(expected_identity, &runtime.inner.default_agent_id)
    );
    assert_eq!(read.summary.current_run_id, root.state.current_run_id);
    assert_eq!(read.summary.pending, root.state.pending);
    assert!(Arc::ptr_eq(&read.material.read.state, &root.state));
    assert_eq!(Arc::strong_count(&root), strong);
    let weak = Arc::downgrade(&read.material.output[0]);
    let (summary, material) = read.into_parts();
    assert!(material.material_estimate()?.0 > 0);
    assert_eq!(
        serde_json::to_value(&summary)?["observability"],
        serde_json::to_value(&expected)?
    );
    drop(summary);
    assert!(weak.upgrade().is_some());
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(material);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn original_child_capture_joins_actual_held_prior_at_exact_byte_and_node_peak_before_clones(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let expected = root.active_identity()?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let prior = (128 * 1024u64, 32usize);
    let prior_guard = ledger.register_owned_bytes(
        root.work.as_ref().unwrap().scope(),
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let prior_body = vec![0u8; usize::try_from(prior.0)?];
    let held = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    deny(&runtime)?;
    let fits = |limits| -> Result<bool> {
        let result =
            runtime.child_summary_from_owner_with_material(&owner, expected, limits, prior);
        let fit = match result {
            Ok(read) => {
                drop(read);
                true
            }
            Err(error) => {
                assert!(exhausted(&error), "{error:#}");
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, held);
        assert_eq!(Arc::strong_count(&root), strong);
        assert!(Arc::ptr_eq(
            &root,
            &owner.capture_ready_snapshot()?.unwrap()
        ));
        Ok(fit)
    };
    assert!(fits(LIMITS)?);
    let mut bytes = (1usize, LIMITS.0);
    while bytes.0 < bytes.1 {
        let middle = bytes.0 + (bytes.1 - bytes.0) / 2;
        if fits((middle, LIMITS.1))? {
            bytes.1 = middle
        } else {
            bytes.0 = middle + 1
        }
    }
    let mut nodes = (1usize, LIMITS.1);
    while nodes.0 < nodes.1 {
        let middle = nodes.0 + (nodes.1 - nodes.0) / 2;
        if fits((bytes.0, middle))? {
            nodes.1 = middle
        } else {
            nodes.0 = middle + 1
        }
    }
    let mut joint_bytes = (1usize, bytes.0);
    while joint_bytes.0 < joint_bytes.1 {
        let middle = joint_bytes.0 + (joint_bytes.1 - joint_bytes.0) / 2;
        if fits((middle, nodes.0))? {
            joint_bytes.1 = middle
        } else {
            joint_bytes.0 = middle + 1
        }
    }
    let exact = (joint_bytes.0, nodes.0);
    assert!(!fits((exact.0 - 1, exact.1))?);
    assert!(!fits((exact.0, exact.1 - 1))?);
    assert!(fits(exact)?);
    let read = runtime.child_summary_from_owner_with_material(&owner, expected, exact, prior)?;
    let own = read.material.material_estimate()?;
    assert!(own.0 + prior.0 <= u64::try_from(exact.0)? && own.1 + prior.1 <= exact.1);
    assert!(own.0 < u64::try_from(exact.0)?);
    drop(read);
    assert_eq!(prior_body.len(), usize::try_from(prior.0)?);
    drop((prior_body, prior_guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn original_child_address_incarnation_and_actual_ledger_reject_without_replacing_publication(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let expected = root.active_identity()?;
    let mut successor = expected.as_ref().clone();
    successor.incarnation += 1;
    let error = runtime
        .child_summary_from_owner_with_material(&owner, &successor, LIMITS, (0, 0))
        .err()
        .context("successor was accepted")?;
    assert!(matches!(
        error.downcast_ref::<ChildAgentSummaryReadError>(),
        Some(ChildAgentSummaryReadError::ForeignMaterial)
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut read =
        runtime.child_summary_from_owner_with_material(&owner, expected, LIMITS, (0, 0))?;
    let weak = Arc::downgrade(&read.material.output[0]);
    read.material.ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let error = read.material.material_estimate().unwrap_err();
    assert!(matches!(
        error.downcast_ref::<ChildAgentSummaryReadError>(),
        Some(ChildAgentSummaryReadError::ForeignMaterial)
    ));
    assert_eq!(read.material.ledger.report()?.observed_bytes, 0);
    assert!(Arc::ptr_eq(
        &root,
        &owner.capture_ready_snapshot()?.unwrap()
    ));
    drop(read);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn original_child_closure_admits_selected_long_work_addresses_before_policy_copies(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let mut work = WorkItemRecord::new(
        runtime.instance_agent_id(),
        "actual long child closure address",
        crate::types::WorkItemState::Open,
    );
    work.id = "original-child-work-address-".repeat(256);
    work.plan_status = crate::types::WorkItemPlanStatus::Ready;
    let commit = {
        let mut mutation = runtime.begin_mutation().await?;
        let execution_read = mutation
            .execution_command_source()?
            .capture(None, &[&work.id])?;
        let protocol = runtime.plan_work_item_execution_create_for_test(&work, &execution_read)?;
        mutation
            .commit_work_transition(
                crate::ids::audit_event_id(),
                crate::runtime_db::transitions::WorkItemTransitionCommand {
                    agent_id: work.agent_id.clone(),
                    mutation: crate::runtime_db::transitions::WorkItemMutation::Insert {
                        record: work.clone(),
                    },
                    agent_state: None,
                    brief_evidence: Vec::new(),
                    audit_events: Vec::new(),
                    index_changes: Vec::new(),
                    notify_scheduler: true,
                    fault: None,
                },
                protocol,
                true,
            )
            .await?
    };
    runtime.apply_transition_commit(commit).await;
    {
        let mut mutation = runtime.begin_mutation().await?;
        drop(
            mutation
                .prepare_scheduler_read(None, Default::default(), Default::default(), runtime.now())
                .await?,
        );
    }
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let expected = root.active_identity()?;
    let identity_cost = (
        root._identity_budget
            .as_ref()
            .unwrap()
            .owned_material_bytes()?,
        24 * 8,
    );
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    deny(&runtime)?;
    let read = runtime.capture_scheduler_read_from_snapshot_with_material(
        &root,
        &super::super::resident_work::WorkCaptureRequest::current(),
        runtime.now(),
        LIMITS,
        identity_cost,
        None,
    )?;
    assert!(read
        .projection
        .current_work_item
        .iter()
        .chain(&read.projection.queued_runnable_work_items)
        .any(|item| item.id == work.id));
    let prior = add(identity_cost, read.material_estimate()?)?;
    let held = ledger.report()?.observed_bytes;
    let output = ChildOutput::new(
        ledger,
        &read.work.scope,
        (u64::try_from(LIMITS.0)?, LIMITS.1),
        prior,
    )?;
    let base = output.cost;
    let closure = closure_material(&read.state, &read.projection)?;
    assert!(closure.0 > u64::try_from(work.id.len())? * 16);
    drop(output);
    assert_eq!(ledger.report()?.observed_bytes, held);
    let exact = add(add(prior, base)?, closure)?;
    for cap in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let mut output = ChildOutput::new(ledger, &read.work.scope, cap, prior)?;
        let error = output
            .closure_waiting_reason(&read.state, &read.projection)
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ChildAgentSummaryReadError>(),
            Some(ChildAgentSummaryReadError::ResourceExhausted)
        ));
        assert_eq!(output.cost, base);
        assert_eq!(
            output.guards.len(),
            1,
            "policy gate failed before new copy material was registered"
        );
        drop(output);
        assert_eq!(ledger.report()?.observed_bytes, held);
    }
    let mut output = ChildOutput::new(ledger, &read.work.scope, exact, prior)?;
    let waiting = output.closure_waiting_reason(&read.state, &read.projection)?;
    assert_eq!(output.cost, add(base, closure)?);
    assert_eq!(
        waiting,
        RuntimeHandle::closure_decision_from_projection(&read.state, &read.projection, None)
            .waiting_reason
    );
    drop(output);
    assert_eq!(ledger.report()?.observed_bytes, held);
    drop(read);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let captured =
        runtime.child_summary_from_owner_with_material(&owner, expected, LIMITS, (0, 0))?;
    assert!(captured.material.output.len() <= 8);
    assert_eq!(Arc::strong_count(&root), strong);
    drop(captured);
    assert!(Arc::ptr_eq(
        &root,
        &owner.capture_ready_snapshot()?.unwrap()
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
