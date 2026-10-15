use super::*;
use crate::runtime::tests::support::{context_config, tempdir};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "compact-primary-read",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn scheduler_exhausted(error: &anyhow::Error) -> bool {
    use crate::runtime::{
        current_read_facts::CurrentReadError, resident_input_evidence::InputCaptureError,
        resident_scheduling::SchedulingReadError, resident_task::TaskReadError,
        resident_work::WorkReadError,
    };
    matches!(
        error.downcast_ref::<CurrentReadError>(),
        Some(CurrentReadError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<InputCaptureError>(),
        Some(InputCaptureError::ResourceExhausted)
    ) || matches!(
        error.downcast_ref::<SchedulingReadError>(),
        Some(SchedulingReadError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<TaskReadError>(),
        Some(TaskReadError::ResourceExhausted(_))
    ) || matches!(
        error.downcast_ref::<WorkReadError>(),
        Some(WorkReadError::ResourceExhausted(_))
    )
}
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);
#[tokio::test]
async fn actual_loaded_compact_primary_uses_one_publication_and_keeps_selected_material_without_sql_or_root_backedge(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let root_count = Arc::strong_count(&root);
    let expected_identity =
        AgentIdentityView::from_record(root.active_identity()?, &runtime.inner.default_agent_id);
    let config = runtime.inner.config_snapshot.load();
    let expected_model = super::super::agent_model_state_for_catalog(
        &config.model_catalog,
        &config.base_context_config,
        &root.state,
    );
    drop(config);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO compact_forbidden_states;
         ALTER TABLE work_items RENAME TO compact_forbidden_work;
         ALTER TABLE tasks RENAME TO compact_forbidden_tasks;
         ALTER TABLE audit_events RENAME TO compact_forbidden_events;
         ALTER TABLE agent_identities RENAME TO compact_forbidden_identity;",
    )?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let mut projection = runtime.lightweight_agent_state_projection().await?;
    assert_eq!(projection.agent, *root.state);
    assert_eq!(projection.identity, expected_identity);
    assert_eq!(
        serde_json::to_value(&projection.model)?,
        serde_json::to_value(&expected_model)?
    );
    assert!(projection.active_children.is_empty());
    let dto = crate::http_dto::SlimAgentDto::from(&projection);
    assert_eq!(dto.agent.id, root.state.id);
    assert_eq!(dto.identity.incarnation, root.token.identity_incarnation);
    assert_eq!(dto.model.effective_model, expected_model.effective_model);
    let material = projection
        ._ordinary_material
        .take()
        .context("loaded compact omitted primary material")?;
    assert!(material.material_estimate()?.0 > 0);
    let weak = Arc::downgrade(&material.output);
    assert_eq!(
        Arc::strong_count(&root),
        root_count,
        "compact retained a Root"
    );
    drop((dto, projection));
    assert!(
        weak.upgrade().is_some(),
        "selected material did not survive its DTO consumer"
    );
    assert!(ledger.report()?.observed_bytes > baseline);
    drop(material);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn actual_compact_parent_and_selected_model_share_exact_material_limits_and_preserve_old_root_on_failure(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    {
        let mut mutation = runtime.begin_mutation().await?;
        mutation.state.last_wake_reason = Some("actual-large-state".repeat(32 * 1024));
        mutation.persist_state(&runtime.inner.storage).await?;
    }
    runtime.refresh_context_resources().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner.capture_ready_snapshot()?.unwrap();
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let measured = runtime.capture_compact_primary_from_owner(&owner, (usize::MAX, usize::MAX))?;
    let actual = measured
        ._ordinary_material
        .as_ref()
        .unwrap()
        .material_estimate()?;
    drop(measured);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let fits = |limits| -> Result<bool> {
        let fits = match runtime.capture_compact_primary_from_owner(&owner, limits) {
            Ok(projection) => {
                drop(projection);
                true
            }
            Err(error) => {
                let primary = matches!(
                    error.downcast_ref::<CompactAgentReadError>(),
                    Some(CompactAgentReadError::ResourceExhausted)
                );
                let model = matches!(error.downcast_ref::<super::super::selected_model_read::SelectedModelReadError>(), Some(super::super::selected_model_read::SelectedModelReadError::ResourceExhausted));
                assert!(
                    primary || model || scheduler_exhausted(&error),
                    "unexpected material failure: {error:#}"
                );
                false
            }
        };
        assert!(Arc::ptr_eq(
            &root,
            &owner.capture_ready_snapshot()?.unwrap()
        ));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        Ok(fits)
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
    // Current serializes the numeric request caps. Freeze the final native cap
    // before finding its exact byte boundary, without assuming digit lengths.
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
    let measured_exact = runtime.capture_compact_primary_from_owner(&owner, exact)?;
    let exact_material = measured_exact
        ._ordinary_material
        .as_ref()
        .unwrap()
        .material_estimate()?;
    assert!(exact_material.0 <= u64::try_from(exact.0)? && exact_material.1 <= exact.1);
    assert!(actual.0 > 0 && actual.1 > 0);
    drop(measured_exact);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut exact_projection = runtime.capture_compact_primary_from_owner(&owner, exact)?;
    assert_eq!(
        exact_projection
            ._ordinary_material
            .as_ref()
            .unwrap()
            .material_estimate()?,
        exact_material
    );
    assert_eq!(
        exact_projection.agent.last_wake_reason,
        root.state.last_wake_reason
    );
    let mut material = exact_projection._ordinary_material.take().unwrap();
    let weak = Arc::downgrade(&material.output);
    material.ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let error = material.material_estimate().unwrap_err();
    assert!(matches!(
        error.downcast_ref::<CompactAgentReadError>(),
        Some(CompactAgentReadError::ForeignMaterial)
    ));
    assert_eq!(material.ledger.report()?.observed_bytes, 0);
    drop((material, exact_projection));
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_compact_borrows_identity_before_scheduler_selectors_and_body_copies() -> Result<()>
{
    let (_home, runtime) = fixture().await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let root = owner
        .capture_ready_snapshot()?
        .context("original Root missing")?;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    let identity_guard = root
        ._identity_budget
        .as_ref()
        .context("actual identity guard missing")?;
    let identity = (identity_guard.owned_material_bytes()?, 24 * 8);
    let now = runtime.now();
    let request = super::super::resident_work::WorkCaptureRequest::current();
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO compact_joint_forbidden_states;
         ALTER TABLE work_items RENAME TO compact_joint_forbidden_work;
         ALTER TABLE tasks RENAME TO compact_joint_forbidden_tasks;
         ALTER TABLE audit_events RENAME TO compact_joint_forbidden_events;
         ALTER TABLE agent_identities RENAME TO compact_joint_forbidden_identity;
         ALTER TABLE messages RENAME TO compact_joint_forbidden_messages;
         ALTER TABLE queue_entries RENAME TO compact_joint_forbidden_queue;",
    )?;
    let fits = |limits| -> Result<bool> {
        let result = runtime.capture_scheduler_read_from_snapshot_with_material(
            &root,
            &request,
            now,
            limits,
            (0, 0),
            None,
        );
        let fits = match result {
            Ok(read) => {
                drop(read);
                true
            }
            Err(error) => {
                assert!(scheduler_exhausted(&error), "{error:#}");
                false
            }
        };
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        assert_eq!(Arc::strong_count(&root), strong);
        Ok(fits)
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
        if fits((LIMITS.0, middle))? {
            nodes.1 = middle
        } else {
            nodes.0 = middle + 1
        }
    }
    assert!(u64::try_from(bytes.0)? > identity.0 && nodes.0 > identity.1);
    for limits in [(bytes.0, LIMITS.1), (LIMITS.0, nodes.0)] {
        assert!(
            fits(limits)?,
            "standalone Scheduler fits its actual request"
        );
        let error = runtime
            .capture_compact_primary_from_owner(&owner, limits)
            .err()
            .context("compact omitted its still-held identity prior")?;
        assert!(scheduler_exhausted(&error), "identity must be counted in the Scheduler phase, not only the later compact DTO: {error:#}");
        assert_eq!(ledger.report()?.observed_bytes, baseline);
        assert_eq!(Arc::strong_count(&root), strong);
        assert!(Arc::ptr_eq(
            &root,
            &owner.capture_ready_snapshot()?.unwrap()
        ));
    }
    let projection = runtime.capture_compact_primary_from_owner(&owner, LIMITS)?;
    let material = projection
        ._ordinary_material
        .as_ref()
        .context("compact material missing")?;
    material.validate_accounting()?;
    assert_eq!(
        projection.identity.incarnation,
        root.token.identity_incarnation
    );
    let weak = Arc::downgrade(&material.output);
    drop(projection);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
