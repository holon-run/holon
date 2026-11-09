use super::*;
use crate::runtime::tests::support::LifecycleHarness;

#[tokio::test]
async fn default_get_agent_does_not_read_mutable_state_for_cleanup_identity() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    owner.begin("pending-default-get-agent".into())?;
    let agent = runtime.inner.agent.lock().await;
    let tree_error = tokio::time::timeout(Duration::from_secs(1), host.operator_agent_tree())
        .await?
        .err()
        .context("pending tree summary must remain typed")?;
    assert!(tree_error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(crate::tool::ToolError::from_anyhow(&tree_error).retryable);
    let list_error = tokio::time::timeout(Duration::from_secs(1), host.list_agents())
        .await?
        .err()
        .context("pending list summary must remain typed")?;
    assert!(list_error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    let call = crate::tool::ToolCall {
        id: "pending-self".into(),
        name: "GetAgent".into(),
        input: serde_json::json!({}),
    };
    let registry = crate::tool::ToolRegistry::new(runtime.workspace_root());
    let mut read = Box::pin(registry.execute(
        &runtime,
        "default",
        &AuthorityClass::OperatorInstruction,
        &call,
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut read)
        .await
        .is_err());
    drop(agent);
    owner.definitely_not_committed()?;
    drop(owner);
    let (result, _) = tokio::time::timeout(Duration::from_secs(5), read).await??;
    assert!(!result.is_error());
    assert_eq!(
        result.envelope.result.unwrap()["agent"]["identity"]["agent_id"],
        runtime.inner.mutation_owner.agent_id()
    );

    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    owner.begin("pending-default-get-agent-exhaustion".into())?;
    let agent = runtime.inner.agent.lock().await;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        registry.execute(
            &runtime,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
        ),
    )
    .await?
    .err()
    .context("pending default summary must remain retryable")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(runtime.inner.mutation_owner.capture().is_err());
    drop(agent);
    owner.definitely_not_committed()?;
    drop(owner);
    drop(refresh);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn runtime_activity_uses_one_observer_cut_without_execution_locks() -> Result<()> {
    let (_home, host, runtime) = crate::runtime::tests::support::host_backed_test_runtime().await;
    runtime.await_initial_resources().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    let expected = runtime.child_agent_summary_observation().await?;
    let mut owner = runtime.inner.mutation_owner.reserve().await?;
    let agent = runtime.inner.agent.lock().await;
    let activity = tokio::time::timeout(
        Duration::from_secs(1),
        host.public_agent_activity_snapshots(),
    )
    .await??;
    let current = activity
        .iter()
        .find(|entry| entry.agent_id == expected.state.id)
        .context("default activity must be visible")?;
    assert_eq!(current.status, expected.state.status);
    assert_eq!(current.active_task_count, expected.active_task_count);
    assert_eq!(
        current.last_runtime_failure,
        expected.state.last_runtime_failure
    );
    owner.begin("pending-runtime-activity".into())?;
    let error = tokio::time::timeout(
        Duration::from_secs(1),
        host.public_agent_activity_snapshots(),
    )
    .await?
    .err()
    .context("pending activity must not wait or reconcile")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(runtime.inner.mutation_owner.capture().is_err());
    drop(agent);
    owner.definitely_not_committed()?;
    drop(owner);
    drop(refresh);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn get_agent_retries_a_pending_target_without_reconciling_its_owner() -> Result<()> {
    let (_home, host, parent) = crate::runtime::tests::support::host_backed_test_runtime().await;
    host.create_named_agent("summary-target", None).await?;
    let target = host.get_or_create_agent("summary-target").await?;
    target.await_initial_resources().await;
    let refresh = target.hold_resource_refresh_for_test().await?;
    let mut owner = target.inner.mutation_owner.reserve().await?;
    owner.begin("pending-get-agent-target".into())?;
    let error = parent
        .agent_summary_for("summary-target")
        .await
        .err()
        .context("a pending target must not return a summary")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    let wire = crate::tool::ToolError::from_anyhow(&error);
    assert_eq!(wire.kind, "runtime_db_busy");
    assert!(wire.retryable);
    let call = crate::tool::ToolCall {
        id: "pending-target".into(),
        name: "GetAgent".into(),
        input: serde_json::json!({"agent_id":"summary-target", "detail":"full"}),
    };
    let mut read = Box::pin(crate::tool::tools::execute_builtin_tool(
        &parent,
        "default",
        &AuthorityClass::OperatorInstruction,
        &call,
    ));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut read)
        .await
        .is_err());
    assert!(target.inner.mutation_owner.capture().is_err());
    owner.definitely_not_committed()?;
    drop(owner);
    let result = tokio::time::timeout(Duration::from_secs(5), read).await??;
    assert!(!result.is_error());
    assert_eq!(
        result.envelope.result.unwrap()["agent"]["identity"]["agent_id"],
        "summary-target"
    );

    let mut owner = target.inner.mutation_owner.reserve().await?;
    owner.begin("get-agent-retry-exhaustion".into())?;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        crate::tool::tools::execute_builtin_tool(
            &parent,
            "default",
            &AuthorityClass::OperatorInstruction,
            &call,
        ),
    )
    .await?
    .err()
    .context("pending target must exhaust the retry budget")?;
    assert!(error.is::<crate::runtime_db::RuntimeDbRetryableError>());
    assert!(crate::tool::ToolError::from_anyhow(&error).retryable);
    assert!(target.inner.mutation_owner.capture().is_err());
    owner.definitely_not_committed()?;
    drop(owner);
    drop(refresh);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn pinned_summary_and_child_cut_survive_a_new_pending_mutation() -> Result<()> {
    use crate::{
        diagnostics::io_probe::DenyResourceReaders, runtime::mutation_owner::MutationReservation,
        runtime_db::RuntimeDbReadScope,
    };
    use std::time::{Duration, Instant};

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let (_, _, _, original_resources) = runtime.summary_scheduler_from_root(&root)?;
    let mut owners = vec![runtime.inner.mutation_owner.reserve().await?];
    MutationReservation::begin_all(&mut owners, "pending-after-summary-capture".into())?;
    let denied = DenyResourceReaders::new();

    let (_, identity, _, resources) = runtime.summary_scheduler_from_root(&root)?;
    assert_eq!(identity.agent_id, root.state.id);
    assert!(original_resources
        .observation_weak()
        .ptr_eq(&resources.observation_weak()));
    let child = runtime.child_summary_from_root(&root, runtime.runtime_db())?;
    assert_eq!(child.state, root.state);
    assert_eq!(denied.attempts(), 0);
    assert!(runtime.inner.mutation_owner.capture().is_err());

    let scope = RuntimeDbReadScope::new(Instant::now() + Duration::from_secs(5));
    scope.cancel();
    let cancelled = runtime.runtime_db().with_read_scope(scope);
    assert!(runtime.child_summary_from_root(&root, &cancelled).is_err());
    MutationReservation::definitely_not_committed_all(&mut owners)?;
    Ok(())
}

#[tokio::test]
async fn fleet_state_decided_posture_still_requires_the_original_current_binding() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;

    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    runtime
        .create_work_item("roster queued input".into(), None, None, Vec::new())
        .await?;
    runtime
        .enqueue(MessageEnvelope::new(
            "default",
            MessageKind::OperatorPrompt,
            MessageOrigin::Operator {
                actor_id: None,
                actor_display_name: None,
            },
            AuthorityClass::OperatorInstruction,
            Priority::Normal,
            MessageBody::Text {
                text: "roster queued input".into(),
            },
        ))
        .await?;
    let mut owner = runtime.begin_mutation().await?;
    owner.prepare_current_query(None).await?;
    let root = owner.capture_published_root()?;
    let material = ResourceReadMaterial::current_or_new_for_identity(
        &runtime.inner.input_evidence_budget,
        &root.state.id,
        root.token.identity_incarnation,
    )?;
    let expected = material.read(|| runtime.roster_list_fields_from_root(&root))?;
    assert_eq!(
        expected.2.posture,
        crate::types::AgentSchedulingPosture::HasQueuedInput
    );
    let mut malformed = root.as_ref().clone();
    malformed.current = None;
    assert!(material
        .read(|| runtime.roster_list_fields_from_root(&malformed))
        .is_err());
    let mut foreign = root.as_ref().clone();
    foreign.token.identity_incarnation += 1;
    assert!(material
        .read(|| runtime.roster_list_fields_from_root(&foreign))
        .is_err());
    material.finish()?;
    Ok(())
}

#[tokio::test]
async fn fleet_loaded_entry_uses_one_ready_cut_without_state_lock_body_sql_or_root_retention(
) -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    drop(runtime.agent_summary().await?);
    let expected = runtime.agent_list_entry().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let identity = root
        .current
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .identity
        .clone();
    let old_root = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(AuditEvent::legacy("roster_cut", serde_json::json!({})))
        .await?;
    assert!(old_root.upgrade().is_none());
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO fleet_forbidden_states;
             ALTER TABLE agent_identities RENAME TO fleet_forbidden_identity;
             ALTER TABLE queue_entries RENAME TO fleet_forbidden_queue;
             ALTER TABLE audit_events RENAME TO fleet_forbidden_events;
             ALTER TABLE briefs RENAME TO fleet_forbidden_briefs;
             ALTER TABLE tasks RENAME TO fleet_forbidden_tasks;
             ALTER TABLE wait_conditions RENAME TO fleet_forbidden_waits;
             ALTER TABLE timers RENAME TO fleet_forbidden_timers;
             ALTER TABLE timer_wakes RENAME TO fleet_forbidden_wakes;
             ALTER TABLE external_triggers RENAME TO fleet_forbidden_triggers;",
        )?;
        Ok(())
    })?;
    let phase = ResourceReadMaterial::current_or_new_for_identity(
        &runtime.inner.input_evidence_budget,
        &identity.agent_id,
        identity.incarnation,
    )?;
    let _agent = runtime.inner.agent.lock().await;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let actual = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        runtime.agent_list_entry_read(&identity, &phase, &runtime.inner.runtime_db),
    )
    .await??;
    assert_eq!(denied.attempts(), 0);
    drop(denied);
    assert_eq!(actual, expected);
    let mut foreign = identity.as_ref().clone();
    foreign.incarnation += 1;
    assert!(runtime
        .agent_list_entry_read(&foreign, &phase, &runtime.inner.runtime_db)
        .await
        .is_err());
    Ok(())
}

#[tokio::test]
async fn fleet_loaded_entry_ignores_reserved_owner_but_observes_cancellation_and_deadline(
) -> Result<()> {
    use crate::{diagnostics::resource_io::ResourceReadMaterial, runtime_db::RuntimeDbReadScope};
    use std::time::{Duration, Instant};
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let root = runtime.capture_observer_root(runtime.runtime_db())?;
    let identity = root
        .current
        .as_ref()
        .unwrap()
        .query
        .as_ref()
        .unwrap()
        .identity
        .clone();
    let phase = ResourceReadMaterial::current_or_new_for_identity(
        &runtime.inner.input_evidence_budget,
        &identity.agent_id,
        identity.incarnation,
    )?;
    let owner = runtime.inner.mutation_owner.reserve().await?;
    let _agent = runtime.inner.agent.lock().await;
    tokio::time::timeout(
        Duration::from_secs(5),
        runtime.agent_list_entry_read(&identity, &phase, runtime.runtime_db()),
    )
    .await??;
    for cancel in [true, false] {
        let deadline = if cancel {
            Instant::now() + Duration::from_secs(30)
        } else {
            Instant::now() - Duration::from_millis(1)
        };
        let scope = RuntimeDbReadScope::new(deadline);
        if cancel {
            scope.cancel();
        }
        let db = runtime.runtime_db().with_read_scope(scope);
        assert!(runtime
            .agent_list_entry_read(&identity, &phase, &db)
            .await
            .is_err());
    }
    drop(owner);
    Ok(())
}

#[tokio::test]
async fn full_summary_ready_cut_avoids_body_sql_agent_lock_and_root_retention() -> Result<()> {
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let mut prepare = runtime.begin_mutation().await?;
    prepare.prepare_current_scheduler_query(None).await?;
    drop(prepare);
    let first = runtime.agent_summary().await?;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let old_root = Arc::downgrade(&root);
    drop(root);
    runtime
        .append_original_audit_event(AuditEvent::legacy(
            "summary_cut",
            serde_json::json!({"keeps_original_cut": true}),
        ))
        .await?;
    assert!(old_root.upgrade().is_none());
    let counts = runtime.inner.storage.summary_record_counts()?;
    runtime.runtime_db().transaction(|tx| {
        tx.execute_batch(
            "ALTER TABLE agent_states RENAME TO summary_forbidden_states;
             ALTER TABLE agent_identities RENAME TO summary_forbidden_identity;
             ALTER TABLE queue_entries RENAME TO summary_forbidden_queue;
             ALTER TABLE audit_events RENAME TO summary_forbidden_events;
             ALTER TABLE briefs RENAME TO summary_forbidden_briefs;
             ALTER TABLE tasks RENAME TO summary_forbidden_tasks;
             ALTER TABLE wait_conditions RENAME TO summary_forbidden_waits;
             ALTER TABLE timers RENAME TO summary_forbidden_timers;
             ALTER TABLE timer_wakes RENAME TO summary_forbidden_wakes;
             ALTER TABLE external_triggers RENAME TO summary_forbidden_triggers;",
        )?;
        Ok(())
    })?;
    let _owner = runtime.inner.mutation_owner.reserve().await?;
    let _agent = runtime.inner.agent.lock().await;
    let denied = crate::diagnostics::io_probe::DenyResourceReaders::new();
    let next =
        tokio::time::timeout(std::time::Duration::from_secs(5), runtime.agent_summary()).await??;
    assert_eq!(denied.attempts(), 0);
    drop(denied);
    assert_eq!(next.agent, first.agent);
    assert_eq!(next.identity, first.identity);
    assert_eq!(next.execution, first.execution);
    assert_eq!(next.active_wait_conditions, first.active_wait_conditions);
    assert_eq!((next.recent_brief_count, next.recent_event_count), counts);
    assert!(Arc::ptr_eq(
        &next.skills.discoverable_skills,
        &next.skills.attached_skills,
    ));
    let observation = next
        .material
        ._resources
        .as_ref()
        .unwrap()
        .observation_weak();
    drop(first);
    let phase = next.material.acquisition.as_ref().unwrap().guard()?;
    let weak = Arc::downgrade(&phase);
    drop(phase);
    let expected = serde_json::to_value(&*next)?;
    let bytes = next.into_json_bytes()?;
    runtime.inner.resources.registry.close()?;
    let cache = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        expected
    );
    drop(bytes);
    assert!(weak.upgrade().is_some());
    assert!(observation.upgrade().is_some());
    drop(cache);
    assert!(weak.upgrade().is_some());
    drop(slice);
    assert!(weak.upgrade().is_none());
    assert!(observation.upgrade().is_none());
    Ok(())
}

#[tokio::test]
async fn full_summary_counts_raw_event_metadata_without_decoding_or_qualifying_body() -> Result<()>
{
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    for _ in 0..105 {
        runtime
            .inner
            .storage
            .append_event(&AuditEvent::legacy("raw_count", serde_json::json!({})))?;
    }
    runtime.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE audit_events SET data_json='corrupt JSON, metadata only' WHERE agent_id=?1",
            [runtime.instance_agent_id()],
        )?;
        Ok(())
    })?;
    let counts = runtime.inner.storage.summary_record_counts()?;
    let summary = runtime.agent_summary().await?;
    assert_eq!(counts.1, 100);
    assert_eq!(
        (summary.recent_brief_count, summary.recent_event_count),
        counts
    );
    let root = runtime.capture_observer_root(runtime.runtime_db())?;
    let query = root.current.as_ref().unwrap().query.as_ref().unwrap();
    assert!(query
        .capture(
            &crate::runtime_db::current_query::CurrentSelection {
                events_after: Some(0),
                ..Default::default()
            },
            &runtime.inner.input_evidence_budget,
        )
        .is_err());
    let mut foreign = root.as_ref().clone();
    foreign.token.identity_incarnation += 1;
    let error = runtime
        .summary_scheduler_from_root(&foreign)
        .err()
        .context("foreign Summary root must fail before projection")?;
    assert!(error.to_string().contains("Summary root scope differs"));
    Ok(())
}

#[tokio::test]
async fn full_summary_wire_one_byte_short_latches_without_changing_prior_bytes() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let harness = LifecycleHarness::new();
    let runtime = harness.runtime();
    runtime.await_initial_resources().await;
    let _refresh = runtime.hold_resource_refresh_for_test().await?;
    let scope = super::super::resident_input_evidence::InputEvidenceScope {
        agent_id: runtime.instance_agent_id().to_owned(),
        identity_incarnation: runtime.inner.identity_incarnation,
    };
    let ledger = &runtime.inner.input_evidence_budget;
    let mut summary = runtime.agent_summary().await?;
    let value = serde_json::to_value(&*summary)?;
    let size = super::super::input_evidence_budget::serialized_bytes(&*summary)?;
    let holder = u64::try_from(std::mem::size_of::<SummaryWire>() + 128)?;
    let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, 1024 * 1024, 8192)?;
    let header = phase.finish()?.0;
    drop(phase);
    let required = header + (holder + size) * 8;
    summary.material.acquisition = Some(ResourceReadMaterial::with_test_limits(
        ledger, &scope, required, 8192,
    )?);
    let bytes = summary.into_json_bytes()?;
    let mut rejected = runtime.agent_summary().await?;
    let phase = ResourceReadMaterial::with_test_limits(ledger, &scope, required - 1, 8192)?;
    rejected.material.acquisition = Some(phase.clone());
    assert!(rejected.into_json_bytes().is_err());
    assert!(phase.finish().is_err());
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes)?, value);
    Ok(())
}
