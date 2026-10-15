use super::*;
use crate::runtime::tests::support::{context_config, tempdir};

async fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    // Pure summary fixtures use their own resource roots and complete the
    // explicit initial observation before ordinary capture.
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "summary-scheduling",
        home.into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok(runtime)
}

#[tokio::test]
async fn summary_trigger_projection_uses_original_scheduler_read_after_revoke_ack() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    let trigger = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let original = runtime.capture_scheduler_projection().await?;
    let original_revision = original.scheduling.revision;
    runtime
        .revoke_external_trigger(&trigger.external_trigger_id)
        .await?;
    let current = runtime.capture_scheduler_projection().await?;
    assert!(current.scheduling.revision > original_revision);
    let (old, old_material) = runtime.summary_triggers_from_capture(original.scheduling, (0, 0))?;
    let (new, new_material) = runtime.summary_triggers_from_capture(current.scheduling, (0, 0))?;
    assert_eq!(old.len(), 1);
    assert_eq!(old[0].external_trigger_id, trigger.external_trigger_id);
    assert_eq!(old[0].status, ExternalTriggerStatus::Active);
    assert!(new.is_empty());
    assert_eq!(serde_json::to_value(&old)?[0]["status"], "active");
    drop((old, new, old_material, new_material));
    Ok(())
}

#[tokio::test]
async fn summary_and_get_agent_wire_hold_original_material_without_scheduling_body_sql(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    let trigger = runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    drop(runtime.agent_summary().await?);
    // The background observer can replace its immutable resource candidate
    // independently. Join it before measuring only this reader's last holder.
    runtime
        .quiesce_context_resource_observer_task_for_test()
        .await?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE external_triggers RENAME TO summary_triggers_forbidden; ALTER TABLE timers RENAME TO summary_timers_forbidden; ALTER TABLE timer_wakes RENAME TO summary_wakes_forbidden")?;
        Ok(())
    })?;
    let summary = runtime.agent_summary().await?;
    let owned = Arc::downgrade(&summary.material._owned[0]);
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    let original_dto = serde_json::to_value(&*summary)?;
    let wire = serde_json::to_value(&summary)?;
    assert_eq!(wire, original_dto);
    assert_eq!(
        wire["active_external_triggers"][0]["external_trigger_id"],
        trigger.external_trigger_id
    );
    assert!(owned.upgrade().is_some());
    drop((summary, wire, original_dto));
    assert!(owned.upgrade().is_none());
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    for detail in ["compact", "full"] {
        let wire = crate::tool::tools::get_agent::execute(
            &runtime,
            runtime.instance_agent_id(),
            &AuthorityClass::RuntimeInstruction,
            &serde_json::json!({"detail": detail}),
        )
        .await?;
        assert!(wire.envelope.result.as_ref().unwrap()["agent"].is_object());
        assert_eq!(
            runtime.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
    }
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch("ALTER TABLE summary_wakes_forbidden RENAME TO timer_wakes; ALTER TABLE summary_timers_forbidden RENAME TO timers; ALTER TABLE summary_triggers_forbidden RENAME TO external_triggers")?;
        Ok(())
    })?;
    runtime
        .revoke_external_trigger(&trigger.external_trigger_id)
        .await?;
    Ok(())
}

#[tokio::test]
async fn summary_projection_rejects_combined_selected_and_output_before_body_clones() -> Result<()>
{
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    drop(runtime.agent_summary().await?);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let scheduling = {
        let read = runtime.capture_scheduler_projection().await?;
        read.scheduling
    };
    let (bytes, records) = scheduling.selected_material_estimate()?;
    let error = project_selected_scheduling_records(
        scheduling,
        0,
        &runtime.inner.input_evidence_budget,
        usize::try_from(bytes)?,
        records,
        (0, 0),
        Vec::new(),
    )
    .err()
    .context("selected alone fits but projected summary must not")?;
    assert!(matches!(
        error.downcast_ref::<super::super::resident_scheduling::SchedulingReadError>(),
        Some(super::super::resident_scheduling::SchedulingReadError::ResourceExhausted(_))
    ));
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn summary_resource_capture_keeps_original_epoch_through_actual_wire_without_file_reads(
) -> Result<()> {
    let home = tempdir()?;
    std::fs::write(home.path().join("AGENTS.md"), "original summary guidance")?;
    std::fs::create_dir_all(home.path().join("skills/summary-original"))?;
    let skill_path = home.path().join("skills/summary-original/SKILL.md");
    std::fs::write(
        &skill_path,
        "---\nname: summary-original\ndescription: original summary skill\n---\nbody",
    )?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "summary-resource-wire",
        home.path().into(),
        context_config(),
    )?;
    runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let pause = runtime.pause_resource_observer_for_test().await;
    let original = crate::diagnostics::resource_io::deny_readers(runtime.agent_summary()).await?;
    let weak = Arc::downgrade(&original.material._resources.as_ref().unwrap().observation);
    let epoch = original
        .material
        ._resources
        .as_ref()
        .unwrap()
        .observation
        .epoch;
    assert_eq!(
        original.skills.discoverable_skills[0].description,
        "original summary skill"
    );
    assert_eq!(
        serde_json::to_value(&original)?,
        serde_json::to_value(&*original)?
    );
    std::fs::write(
        &skill_path,
        "---\nname: summary-original\ndescription: refreshed summary skill\n---\nbody",
    )?;
    runtime.invalidate_context_resources(
        super::super::resource_observation::ResourceInvalidation::KnownWrite,
    )?;
    drop(pause);
    runtime.refresh_context_resources().await?;
    let fresh = crate::diagnostics::resource_io::deny_readers(runtime.agent_summary()).await?;
    assert!(
        fresh
            .material
            ._resources
            .as_ref()
            .unwrap()
            .observation
            .epoch
            > epoch
    );
    assert_eq!(
        fresh.skills.discoverable_skills[0].description,
        "refreshed summary skill"
    );
    assert_eq!(
        original.skills.discoverable_skills[0].description,
        "original summary skill"
    );
    assert!(weak.upgrade().is_some());
    let response =
        crate::http::traced_json("summary-resource-wire", std::time::Instant::now(), original)
            .map_err(|error| anyhow!("{error:?}"))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert!(
        weak.upgrade().is_none(),
        "actual serializer released its old observation"
    );
    Ok(())
}

#[tokio::test]
async fn summary_and_skills_do_not_convert_dirty_resources_into_empty_catalogs() -> Result<()> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "summary-resource-dirty",
        home.path().into(),
        context_config(),
    )?;
    runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    runtime.invalidate_context_resources(
        super::super::resource_observation::ResourceInvalidation::KnownWrite,
    )?;
    for error in [
        runtime.agent_summary().await.err(),
        runtime.capture_agent_skills_read().await.err(),
    ] {
        let error = error.context("Dirty resource must fail the dependent public projection")?;
        assert!(matches!(
            error.downcast_ref::<super::super::resource_observation::ResourceObservationError>(),
            Some(
                super::super::resource_observation::ResourceObservationError::NotReady(
                    super::super::resource_observation::ResourceFreshness::Dirty(_)
                )
            )
        ));
    }
    Ok(())
}

#[tokio::test]
async fn summary_resource_descriptor_uses_remaining_budget_before_owned_allocation() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    let mutation = runtime.begin_mutation().await?;
    let root = mutation.capture_published_root()?;
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    let error = runtime
        .capture_context_resources_for_snapshot_with_limits(
            &root.state,
            root.work.as_deref(),
            None,
            0,
            0,
        )
        .err()
        .context("source descriptor must use the caller's remaining allowance")?;
    assert!(
        matches!(
            error.downcast_ref::<super::super::resource_observation::ResourceObservationError>(),
            Some(
                super::super::resource_observation::ResourceObservationError::ResourceExhausted(
                    "source descriptor"
                )
            )
        ),
        "{error:#}"
    );
    assert_eq!(
        runtime.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    Ok(())
}

#[tokio::test]
async fn summary_trigger_output_debits_other_held_domains_before_wire_allocation() -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    let read = runtime.capture_scheduler_projection().await?;
    let selected = read.scheduling.selected_material_estimate()?;
    let limits = runtime.inner.input_evidence_budget.limits();
    let cap = limits
        .history_per_agent_bytes
        .checked_add(limits.pending_per_agent_bytes)
        .context("fixture limit overflow")?
        .min(64 * 1024 * 1024);
    let baseline = runtime.inner.input_evidence_budget.report()?.observed_bytes;
    // Other material plus the already held selection fits exactly; the actual
    // additional Trigger output must still be refused before construction.
    let error = runtime
        .summary_triggers_from_capture(
            read.scheduling,
            (
                usize::try_from(
                    cap.checked_sub(selected.0)
                        .context("fixture selection too large")?,
                )?,
                0,
            ),
        )
        .err()
        .context("other held domains must leave room for the actual output")?;
    assert!(
        matches!(
            error.downcast_ref::<super::super::resident_scheduling::SchedulingReadError>(),
            Some(super::super::resident_scheduling::SchedulingReadError::ResourceExhausted(_))
        ),
        "{error:#}"
    );
    assert!(runtime.inner.input_evidence_budget.report()?.observed_bytes <= baseline);
    Ok(())
}

#[tokio::test]
async fn summary_identity_is_from_its_original_selected_publication_after_metadata_ack(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    let SummaryProjectionRead {
        read,
        resources,
        owned,
        trigger_prior: _prior,
        identity,
        model,
        model_material,
    } = runtime.capture_summary_projection().await?;
    let old_name = identity.name.clone();
    let guard = Arc::downgrade(&owned);
    let mut actual = runtime
        .runtime_db()
        .agent_identities()
        .latest(runtime.instance_agent_id())?
        .context("actual standalone identity")?;
    actual.name = Some("next actual metadata publication".into());
    actual.revision += 1;
    let mut reservations = vec![runtime.inner.mutation_owner.reserve().await?];
    super::super::mutation_owner::MutationReservation::begin_all(
        &mut reservations,
        crate::ids::audit_event_id(),
    )?;
    runtime.runtime_db().transaction(|tx| {
        crate::runtime_db::evidence::upsert_agent_identity_tx(tx, &actual)?;
        Ok(())
    })?;
    let facts = super::super::mutation_owner::IdentityMutationFacts::budgeted_owned(
        Some(actual),
        Some(read.state.clone()),
        &runtime.inner.input_evidence_budget,
    )?;
    super::super::mutation_owner::MutationReservation::committed_all(
        &mut reservations,
        vec![super::super::mutation_owner::KnownCommit::Metadata(
            Arc::new(facts),
        )],
    )?;
    super::super::mutation_owner::MutationReservation::publish_all_known(&mut reservations)?;
    drop(reservations);
    let (current, _current_guard) = runtime.capture_active_identity_read().await?;
    assert_eq!(
        current.name.as_deref(),
        Some("next actual metadata publication")
    );
    assert_eq!(identity.name, old_name);
    let wire = serde_json::to_value(&identity)?;
    assert_eq!(wire["name"].as_str(), old_name.as_deref());
    assert!(guard.upgrade().is_some());
    drop((
        read,
        resources,
        owned,
        identity,
        wire,
        model,
        model_material,
    ));
    assert!(guard.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn loaded_full_summary_selects_original_model_in_its_parent_phase_without_sql_or_root_retention(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let expected = runtime.model_state_for(&root.state);
    let identity = root.active_identity()?;
    let expected_identity =
        crate::types::AgentIdentityView::from_record(identity, &runtime.inner.default_agent_id);
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let strong = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO summary_model_forbidden_states;
         ALTER TABLE agent_identities RENAME TO summary_model_forbidden_identities;
         ALTER TABLE work_items RENAME TO summary_model_forbidden_work;
         ALTER TABLE tasks RENAME TO summary_model_forbidden_tasks;
         ALTER TABLE messages RENAME TO summary_model_forbidden_messages;
         ALTER TABLE audit_events RENAME TO summary_model_forbidden_events;",
    )?;
    let selected = runtime.capture_summary_projection().await?;
    assert_eq!(selected.model, expected);
    assert_eq!(selected.identity, expected_identity);
    assert!(Arc::ptr_eq(&selected.read.state, &root.state));
    selected
        .model_material
        .validate_accounting(&selected.read.work.scope, ledger)?;
    let own_model = selected.model_material.material_estimate();
    let own_read = selected.read.material_estimate()?;
    assert!(own_model.0 > 0 && own_model.1 > 0);
    let scheduling = selected.read.scheduling.selected_material_estimate()?;
    assert!(u64::try_from(selected.trigger_prior.0)? + scheduling.0 >= own_read.0 + own_model.0);
    assert!(selected.trigger_prior.1 + scheduling.1 >= own_read.1 + own_model.1);
    assert_eq!(Arc::strong_count(&root), strong);
    let model_holder = Arc::downgrade(&selected.model_material);
    drop(selected);
    assert!(model_holder.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
