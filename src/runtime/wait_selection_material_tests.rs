use super::*;
use crate::runtime::tests::support::{context_config, tempdir};

#[tokio::test]
async fn wait_selection_admits_original_snapshot_then_releases_it_before_actual_sql() -> Result<()>
{
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "wait-selection-original-material",
        home.path().into(),
        context_config(),
    )?;
    let created = runtime
        .create_work_item_owned_result(
            "actual selected Wait owner".into(),
            None,
            Some("original plan".into()),
            Vec::new(),
        )
        .await?;
    let id = created.into_parts().0.id;
    let owner = runtime.begin_mutation().await?;
    let selection = runtime.capture_work_query_snapshot_from_owner(
        &owner,
        false,
        None,
        (64 * 1024 * 1024, 65_536),
    )?;
    let old_guard = Arc::downgrade(&selection._owned);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let reason = "actual Wait disclosure ".repeat(1024);
    let borrowed = (&id, &reason, runtime.instance_agent_id());
    let cost = selection.material_estimate()?;
    let error = runtime
        .retain_wait_selection_with_limits(
            &selection,
            &borrowed,
            (
                usize::try_from(cost.0)?
                    .checked_add(1)
                    .context("fixture cap")?,
                65_536,
            ),
        )
        .err()
        .context("snapshot plus first owner copy must reject")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let metadata = runtime.retain_wait_selection_material(&selection, &borrowed)?;
    let scope = selection.work.scope.clone();
    let metadata_cost =
        metadata.validate_accounting(&scope, &runtime.inner.input_evidence_budget)?;
    let weak = Arc::downgrade(&metadata);
    let loan = super::super::WorkResourceMaterialLoan::with_wait_metadata(None, metadata.clone());
    assert_eq!(
        loan.validate_accounting(&scope, &runtime.inner.input_evidence_budget)?,
        metadata_cost
    );
    drop(selection);
    assert!(
        old_guard.upgrade().is_none(),
        "original large publication is no longer retained"
    );
    drop(owner);
    let (outcome, read, registered_metadata) = runtime
        .register_wait_for_outcome_with_selection(
            runtime.instance_agent_id(),
            Some(id.clone()),
            super::super::WaitForWakeKind::OperatorInput,
            None,
            reason,
            None,
            &metadata,
        )
        .await?;
    let super::super::WaitForRegistrationOutcome::Registered { registration } = outcome else {
        anyhow::bail!("actual Wait did not register");
    };
    assert_eq!(
        registration.condition.work_item_id.as_deref(),
        Some(id.as_str())
    );
    let read = read.context("actual Work Wait result")?;
    runtime.stop_context_resource_observer().await?;
    let owner = runtime.begin_mutation().await?;
    let snapshot = runtime.capture_work_query_snapshot_with_metadata_from_owner(
        &owner,
        false,
        Some(read.material()),
        Some(&registered_metadata),
        (64 * 1024 * 1024, 65_536),
    )?;
    assert!(snapshot.material_estimate()?.0 > u64::try_from(metadata_cost.0)?);
    drop(metadata);
    drop(registered_metadata);
    drop(loan);
    drop(read);
    assert!(weak.upgrade().is_some());
    let wire = serde_json::to_value((&snapshot.items, &registration.condition))?;
    assert!(
        weak.upgrade().is_some(),
        "actual metadata guard crosses synchronous serializer"
    );
    drop(wire);
    drop(snapshot);
    assert!(
        weak.upgrade().is_none(),
        "last selected wire holder releases small carrier"
    );
    drop(owner);
    Ok(())
}

#[tokio::test]
async fn wait_metadata_never_borrows_a_foreign_runtime_ledger_or_scope() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "wait-selection-foreign",
        home.path().into(),
        context_config(),
    )?;
    runtime.stop_context_resource_observer().await?;
    let owner = runtime.begin_mutation().await?;
    let selection = runtime.capture_work_query_snapshot_from_owner(
        &owner,
        false,
        None,
        (64 * 1024 * 1024, 65_536),
    )?;
    let metadata =
        runtime.retain_wait_selection_material(&selection, &"original actual selection")?;
    let other_home = tempdir()?;
    let other = RuntimeHandle::new_isolated_resource_fixture(
        "wait-selection-other",
        other_home.path().into(),
        context_config(),
    )?;
    other.stop_context_resource_observer().await?;
    let other_owner = other.begin_mutation().await?;
    let baseline = other.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(other
        .retain_wait_selection_material(&selection, &"foreign selector")
        .is_err());
    assert!(other
        .capture_work_query_snapshot_with_metadata_from_owner(
            &other_owner,
            false,
            None,
            Some(&metadata),
            (64 * 1024 * 1024, 65_536)
        )
        .is_err());
    assert_eq!(
        other.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    drop(other_owner);
    drop(owner);
    Ok(())
}

#[tokio::test]
async fn actual_cancelled_wait_ids_are_admitted_before_return_dto_copies() -> Result<()> {
    use crate::types::{WaitConditionKind, WaitConditionRecord, WaitConditionStatus};
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "wait-actual-cancelled-material",
        home.path().into(),
        context_config(),
    )?;
    let created = runtime
        .create_work_item_owned_result(
            "actual replacement owner".into(),
            None,
            Some("original plan".into()),
            Vec::new(),
        )
        .await?;
    let id = created.into_parts().0.id;
    let at = chrono::Utc::now();
    let waits = (0..1)
        .map(|index| WaitConditionRecord {
            id: format!("actual-{index}-{}", "w".repeat(16 * 1024)),
            agent_id: runtime.instance_agent_id().into(),
            work_item_id: Some(id.clone()),
            status: WaitConditionStatus::Active,
            kind: WaitConditionKind::Operator,
            source: Some("original-typed-wait-fixture".into()),
            subject_ref: None,
            waiting_for: "original waiter".into(),
            wake_sources: Vec::new(),
            continuation: None,
            created_at: at,
            updated_at: at,
            expires_at: None,
            resolved_at: None,
            cancelled_at: None,
            turn_id: None,
            trigger_message_id: None,
            triggered_at: None,
        })
        .collect::<Vec<_>>();
    let mut owner = runtime.begin_mutation().await?;
    let commit = owner
        .commit_wait_transition(
            crate::ids::audit_event_id(),
            crate::runtime_db::transitions::WaitTransitionCommand {
                agent_id: runtime.instance_agent_id().into(),
                work_items: Vec::new(),
                expected_wait_conditions: Vec::new(),
                wait_conditions: waits,
                timer_wake: None,
                task_result_admission: None,
                agent_state: None,
                audit_events: Vec::new(),
                index_changes: Vec::new(),
                notify_scheduler: true,
                fault: None,
            },
            Default::default(),
            None,
        )
        .await?;
    drop(owner);
    runtime.apply_transition_commit(commit).await;
    let owner = runtime.begin_mutation().await?;
    let selection = runtime.capture_work_query_snapshot_from_owner(
        &owner,
        false,
        None,
        (64 * 1024 * 1024, 65_536),
    )?;
    let metadata = runtime.retain_wait_selection_material(&selection, &(&id, "replacement"))?;
    drop(selection);
    drop(owner);
    let outcome = runtime
        .prepare_wait_for_outcome(
            runtime.instance_agent_id(),
            Some(id),
            super::super::WaitForWakeKind::OperatorInput,
            None,
            "replacement".into(),
            None,
        )
        .await?;
    let super::super::PrepareWaitForOutcome::Prepared(mut prepared) = outcome else {
        anyhow::bail!("actual Wait replacement was not prepared");
    };
    assert_eq!(prepared.registration.cancelled_wait_condition_ids.len(), 1);
    assert!(prepared
        .registration
        .cancelled_wait_condition_ids
        .iter()
        .all(|id| id.len() > 16 * 1024));
    let prior = metadata.cost.0;
    let added = WorkQueryMaterial::typed(&prepared.registration, 8)?;
    assert!(
        added.0 > 64 * 1024,
        "actual returned DTO exceeds the old fixed reservation"
    );
    let limit = prior
        .max(added.0)
        .checked_add(1024)
        .context("fixture cap overflow")?;
    assert!(prior < limit && added.0 < limit && prior + added.0 > limit);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .retain_wait_registration_with_limits(
            &metadata,
            &prepared.registration,
            None,
            (limit, 65_536),
        )
        .err()
        .context("actual cancelled IDs must participate in combined cap")?;
    assert!(
        matches!(
            error.downcast_ref::<WorkReadError>(),
            Some(WorkReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    prepared.retain_owner_selection_material(metadata);
    prepared.prepare_owner_selection_result_material(&runtime)?;
    let expanded = prepared
        .owner_selection_result_material()
        .context("actual DTO material")?;
    assert!(expanded.cost.0 >= prior + added.0);
    let weak = Arc::downgrade(expanded);
    let holder = prepared.clone();
    drop(prepared);
    let wire = serde_json::to_value(&holder.registration)?;
    assert!(weak.upgrade().is_some());
    drop(wire);
    drop(holder);
    assert!(weak.upgrade().is_none());
    runtime.stop_context_resource_observer().await?;
    Ok(())
}
