use super::{super::*, test_host};
use crate::types::{AgentOwnership, AgentProfilePreset};

#[tokio::test]
async fn full_cold_summary_wire_preserves_children_without_starting_runtimes() -> Result<()> {
    let (_home, host) = test_host();
    let parent = host.ensure_default_agent_identity()?;
    let selected = child(&host, "summary-wire-child", &parent.agent_id)?;
    host.runtime_db()
        .agent_states()
        .initialize_cold_for_fixture(&AgentState::new(&selected.agent_id), selected.incarnation)?;
    let summary = host
        .local_agent_summary(&parent.agent_id)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    assert_eq!(summary.active_children.len(), 1);
    let guards = [
        summary.active_children[0]
            .observability
            ._read_material
            .as_ref(),
        summary.active_children[0]
            .observability
            ._source_material
            .as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(Arc::downgrade)
    .collect::<Vec<_>>();
    assert!(!guards.is_empty());
    let expected = serde_json::to_value(&*summary)?;
    let bytes = summary.into_json_bytes()?;
    let cache = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        expected
    );
    drop(bytes);
    assert!(guards.iter().all(|guard| guard.upgrade().is_some()));
    drop(cache);
    assert!(guards.iter().all(|guard| guard.upgrade().is_some()));
    drop(slice);
    assert!(guards.iter().all(|guard| guard.upgrade().is_none()));
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn child_inventory_admits_only_selected_identities_and_latches_exact_limits() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    let (_home, host) = test_host();
    let parent = host.config().default_agent_id.clone();
    let selected = child(&host, "inventory-selected", &parent)?;
    let scope = InputEvidenceScope {
        agent_id: selected.agent_id.clone(),
        identity_incarnation: selected.incarnation,
    };
    let ledger = &host.inner.input_evidence_budget;
    let measure = ResourceReadMaterial::with_test_limits(ledger, &scope, 4 * 1024 * 1024, 8192)?;
    let reads = measure.read(|| host.inner.registry.child_identity_reads(&parent, ledger))?;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, selected);
    let (bytes, records) = measure.finish()?;
    drop(reads);
    drop(measure);

    let mut unrelated = child(&host, "inventory-other-parent", "another-parent")?;
    unrelated.name = Some("unselected inventory 中文".repeat(8192));
    host.append_agent_identity(&unrelated)?;
    let mut deleting = child(&host, "inventory-deleting", &parent)?;
    deleting.status = AgentRegistryStatus::Deleting;
    deleting.name = Some("deleting inventory 中文".repeat(8192));
    host.append_agent_identity(&deleting)?;

    let exact = ResourceReadMaterial::with_test_limits(ledger, &scope, bytes, records)?;
    let reads = exact.read(|| host.inner.registry.child_identity_reads(&parent, ledger))?;
    assert_eq!(reads.len(), 1);
    assert_eq!(reads[0].0, selected);
    assert_eq!(exact.finish()?, (bytes, records));
    drop(reads);
    drop(exact);
    for (byte_limit, record_limit) in [(bytes - 1, records), (bytes, records - 1)] {
        let phase =
            ResourceReadMaterial::with_test_limits(ledger, &scope, byte_limit, record_limit)?;
        let error = phase
            .read(|| host.inner.registry.child_identity_reads(&parent, ledger))
            .unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
        assert!(phase.finish().is_err());
        assert!(phase
            .read(|| host.inner.registry.child_identity_reads(&parent, ledger))
            .is_err());
    }
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn state_wire_keeps_child_inventory_and_body_material_through_cache_and_slice() -> Result<()>
{
    for parent_loaded in [false, true] {
        for child_loaded in [false, true] {
            let (_home, host) = test_host();
            let parent = host.config().default_agent_id.clone();
            host.ensure_default_agent_identity()?;
            let identity = child(&host, "wire-child", &parent)?;
            host.runtime_db()
                .agent_states()
                .initialize_cold_for_fixture(
                    &AgentState::new(&identity.agent_id),
                    identity.incarnation,
                )?;
            if parent_loaded {
                host.get_or_create_agent(&parent).await?;
            }
            if child_loaded {
                host.get_or_create_agent(&identity.agent_id).await?;
            }
            let AgentStateReadProjection {
                agent, material, ..
            } = host
                .operator_agent_state_projection(&parent, 10, 10)
                .await
                .map_err(|error| anyhow!("{error}"))?;
            assert_eq!(agent.active_children.len(), 1);
            let weak = agent
                .active_children
                .iter()
                .flat_map(|child| {
                    [
                        child.observability._read_material.as_ref(),
                        child.observability._source_material.as_ref(),
                    ]
                    .into_iter()
                    .flatten()
                    .map(Arc::downgrade)
                })
                .collect::<Vec<_>>();
            assert!(!weak.is_empty());
            if child_loaded {
                assert_eq!(weak.len(), 2, "loaded child retains its inventory too");
            }
            let expected = serde_json::to_vec(&agent.active_children)?;
            let bytes = material.into_json_bytes(&agent.active_children)?;
            let cached = bytes.clone();
            let slice = bytes.slice(..);
            drop(agent);
            drop(bytes);
            assert_eq!(slice.as_ref(), expected);
            assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
            drop(cached);
            assert!(weak.iter().all(|guard| guard.upgrade().is_some()));
            drop(slice);
            assert!(weak.iter().all(|guard| guard.upgrade().is_none()));
            host.shutdown().await?;
        }
    }
    Ok(())
}

fn child(host: &RuntimeHost, id: &str, parent: &str) -> Result<AgentIdentityRecord> {
    let mut identity = AgentIdentityRecord::new(
        id,
        AgentKind::Child,
        AgentVisibility::Private,
        AgentOwnership::ParentSupervised,
        AgentProfilePreset::PrivateChild,
        Some(parent.into()),
        Some(format!("task-{id}")),
    )
    .with_lineage_parent_agent_id(Some(parent.into()));
    identity.name = Some(format!("子代理 {id} 🦀"));
    host.append_agent_identity(&identity)?;
    Ok(identity)
}

// Original caller/assembly path, including its reads and required shared-State
// copy. Compare complete DTOs rather than reconstructing selected fields.
async fn borrowed_summaries(host: &RuntimeHost, parent: &str) -> Result<Vec<ChildAgentSummary>> {
    let mut children = Vec::new();
    for identity in host.agent_identity_records()?.into_iter().filter(|record| {
        record.status == AgentRegistryStatus::Active
            && record.kind == AgentKind::Child
            && record.parent_agent_id.as_deref() == Some(parent)
    }) {
        if let Some(runtime) = host.try_get_loaded_runtime(&identity.agent_id).await {
            let read = runtime.child_agent_summary_observation().await?;
            let (state, active_task_count, observability) =
                (read.state, read.active_task_count, read.observability);
            children.push(ChildAgentSummary {
                identity: AgentIdentityView::from_record(
                    &identity,
                    &host.config().default_agent_id,
                ),
                status: state.status.clone(),
                current_run_id: state.current_run_id.clone(),
                pending: state.pending,
                active_task_count,
                observability,
            });
            continue;
        }
        let storage = host.agent_storage_read_only(&identity.agent_id)?;
        let state = storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(identity.agent_id.clone()));
        let active_task_count = storage.active_task_count_for_agent(&identity.agent_id)?;
        let material =
            crate::diagnostics::resource_io::ResourceReadMaterial::current_or_new_for_identity(
                &host.inner.input_evidence_budget,
                &identity.agent_id,
                identity.incarnation,
            )?;
        children.push(ChildAgentSummary {
            identity: AgentIdentityView::from_record(&identity, &host.config().default_agent_id),
            status: state.status.clone(),
            current_run_id: state.current_run_id.clone(),
            pending: state.pending,
            active_task_count,
            observability: host
                .child_agent_observability_snapshot(&identity.agent_id, &storage, &state, &material)
                .await?,
        });
    }
    children.sort_by(|left, right| left.identity.agent_id.cmp(&right.identity.agent_id));
    Ok(children)
}

fn assert_summaries(actual: &[ChildAgentSummary], expected: &[ChildAgentSummary]) -> Result<()> {
    assert_eq!(actual, expected);
    assert_eq!(
        serde_json::to_value(actual)?,
        serde_json::to_value(expected)?
    );
    Ok(())
}

#[tokio::test]
async fn cold_child_source_acquisition_latches_and_follows_the_returned_snapshot() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    use crate::runtime::resident_input_evidence::InputEvidenceScope;
    let (_home, host) = test_host();
    let parent = host.config().default_agent_id.clone();
    let identity = child(&host, "child-source-holder", &parent)?;
    let mut state = AgentState::new(&identity.agent_id);
    state.last_wake_reason = Some("original cold State 中文".repeat(512));
    host.runtime_db()
        .agent_states()
        .initialize_cold_for_fixture(&state, identity.incarnation)?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let summaries = host.child_agent_summaries(&parent).await?;
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    let material = summaries[0].observability._read_material.as_ref().unwrap();
    let weak = Arc::downgrade(material);
    let clone = summaries[0].observability.clone();
    let wire = serde_json::to_value(&clone)?;
    assert!(wire.get("_read_material").is_none());
    assert!(wire.get("_source_material").is_none());
    let debug = format!("{clone:?}");
    assert!(!debug.contains("_read_material"));
    assert!(!debug.contains("_source_material"));
    assert!(!debug.contains("BudgetViewGuard"));
    assert_eq!(clone, summaries[0].observability);
    drop(summaries);
    assert!(weak.upgrade().is_none());
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > baseline);
    drop(clone);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id.clone(),
        identity_incarnation: identity.incarnation,
    };
    let phase = ResourceReadMaterial::with_test_limits(
        &host.inner.input_evidence_budget,
        &scope,
        (scope.agent_id.len() as u64 + 256) * 8,
        64,
    )?;
    {
        let _entry = phase.enter();
        let error = host.child_agent_summaries(&parent).await.unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
        assert!(phase.finish().is_err());
        assert!(host.child_agent_summaries(&parent).await.is_err());
    }
    drop(phase);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_source_guard_survives_a_loaded_observability_handoff() -> Result<()> {
    use crate::diagnostics::resource_io::ResourceReadMaterial;
    let (_home, host) = test_host();
    let parent = host.config().default_agent_id.clone();
    let task = super::test_child_supervision_task(&parent, "source-handoff");
    let identity = host
        .create_child_identity(&parent, &task, None, &host.agent_data_dir(&parent))
        .await?;
    let storage = host.agent_storage_read_only(&identity.agent_id)?;
    let material = ResourceReadMaterial::current_or_new_for_identity(
        &host.inner.input_evidence_budget,
        &identity.agent_id,
        identity.incarnation,
    )?;
    let state = material.read(|| {
        Ok(storage
            .read_agent()?
            .unwrap_or_else(|| AgentState::new(&identity.agent_id)))
    })?;
    let source = material.guard()?;
    let weak = Arc::downgrade(&source);
    assert!(host
        .try_get_loaded_runtime(&identity.agent_id)
        .await
        .is_none());
    let runtime = host.get_or_create_agent(&identity.agent_id).await?;
    runtime.wait_for_bootstrap().await?;
    let snapshot = host
        .child_agent_observability_snapshot(&identity.agent_id, &storage, &state, &material)
        .await?;
    assert!(Arc::ptr_eq(
        snapshot._source_material.as_ref().unwrap(),
        &source
    ));
    assert!(!Arc::ptr_eq(
        snapshot._read_material.as_ref().unwrap(),
        &source
    ));
    let clone = snapshot.clone();
    assert_eq!(snapshot, clone);
    assert_eq!(
        serde_json::to_value(&snapshot)?,
        serde_json::to_value(&clone)?
    );
    let first = Arc::new(snapshot);
    let last = first.clone();
    drop((material, source, state, first, clone));
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert!(ResourceReadMaterial::active_guard()?.is_none());
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_child_summary_moves_preserve_all_fields_filtering_and_order() -> Result<()> {
    for status in [
        AgentStatus::Booting,
        AgentStatus::AwakeIdle,
        AgentStatus::AwakeRunning,
        AgentStatus::AwaitingTask,
        AgentStatus::Asleep,
        AgentStatus::Stopped,
    ] {
        let (_home, host) = test_host();
        let parent = host.config().default_agent_id.clone();
        for (id, run) in [
            ("z-child", None),
            ("b-child", Some("")),
            ("a-child", Some("run 汉字 🦀")),
        ] {
            let identity = child(&host, id, &parent)?;
            let mut state = AgentState::new(id);
            state.status = status.clone();
            state.current_run_id = run.map(str::to_owned);
            state.last_wake_reason = Some("non-retained State material".repeat(31));
            host.runtime_db()
                .agent_states()
                .initialize_cold_for_fixture(&state, identity.incarnation)?;
        }
        let mut missing = child(&host, "missing-state", &parent)?;
        missing.name = Some(String::new());
        missing.lineage_parent_agent_id = None;
        missing.delegated_from_task_id = None;
        host.append_agent_identity(&missing)?;
        child(&host, "foreign-child", "other-parent")?;
        let mut deleted = child(&host, "deleted-child", &parent)?;
        deleted.status = AgentRegistryStatus::Deleted;
        deleted.revision += 1;
        deleted.updated_at += chrono::Duration::nanoseconds(1);
        host.append_agent_identity(&deleted)?;
        let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
        let expected = borrowed_summaries(&host, &parent).await?;
        let actual = host.child_agent_summaries(&parent).await?;
        assert_summaries(&actual, &expected)?;
        assert_eq!(
            actual
                .iter()
                .map(|child| child.identity.agent_id.as_str())
                .collect::<Vec<_>>(),
            ["a-child", "b-child", "missing-state", "z-child"]
        );
        assert_eq!(actual[0].current_run_id.as_deref(), Some("run 汉字 🦀"));
        assert_eq!(actual[1].current_run_id.as_deref(), Some(""));
        assert_eq!(actual[2].status, AgentStatus::Booting);
        assert!(actual[2].current_run_id.is_none());
        assert!(host.child_agent_summaries("no-parent").await?.is_empty());
        assert!(host.inner.runtimes.read().await.agents.is_empty());
        drop(actual);
        drop(expected);
        assert_eq!(
            host.inner.input_evidence_budget.report()?.observed_bytes,
            baseline
        );
        host.shutdown().await?;
    }
    Ok(())
}

#[tokio::test]
async fn loaded_and_cold_child_summaries_keep_shared_state_and_observation() -> Result<()> {
    let (_home, host) = test_host();
    let parent = host.config().default_agent_id.clone();
    let task = super::test_child_supervision_task(&parent, "summary-owned-identity");
    let loaded = host
        .create_child_identity(&parent, &task, None, &host.agent_data_dir(&parent))
        .await?;
    child(&host, "cold-peer", &parent)?;
    let runtime = host.get_or_create_agent(&loaded.agent_id).await?;
    runtime.wait_for_bootstrap().await?;
    // Keep unrelated owner publications and resource acquisition out of this
    // comparison of summary holders on one immutable loaded publication.
    let run_loop = runtime.pause_run_loop_for_monitor_test().await;
    let refresh = runtime.hold_resource_refresh_for_test().await?;
    let read = runtime.child_agent_summary_observation().await?;
    let (shared, count, observability) = (read.state, read.active_task_count, read.observability);
    let original = serde_json::to_value(shared.as_ref())?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let expected = borrowed_summaries(&host, &parent).await?;
    let actual = host.child_agent_summaries(&parent).await?;
    assert_summaries(&actual, &expected)?;
    assert_eq!(actual.len(), 2);
    let summary = actual
        .iter()
        .find(|child| child.identity.agent_id == loaded.agent_id)
        .unwrap();
    assert_eq!(summary.active_task_count, count);
    assert_eq!(summary.observability, observability);
    assert_eq!(summary.current_run_id, shared.current_run_id);
    assert_eq!(serde_json::to_value(shared.as_ref())?, original);
    assert!(Arc::strong_count(&shared) > 1);
    drop(actual);
    drop(expected);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    drop((refresh, run_loop));
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn child_summary_moves_keep_parent_holder_and_fail_without_partial_return() -> Result<()> {
    let (_home, host) = test_host();
    let parent = host.config().default_agent_id.clone();
    child(&host, "child-holder", &parent)?;
    let identity = child(&host, "child-error", &parent)?;
    let state = AgentState::new(&identity.agent_id);
    host.runtime_db()
        .agent_states()
        .initialize_cold_for_fixture(&state, identity.incarnation)?;
    let expected = borrowed_summaries(&host, &parent).await?;
    let baseline = host.inner.input_evidence_budget.report()?.observed_bytes;
    let read = host
        .local_agent_summary(&parent)
        .await
        .map_err(|error| anyhow!("{error}"))?;
    assert_summaries(&read.active_children, &expected)?;
    let held = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > baseline);
    let first = Arc::new(read);
    let last = first.clone();
    drop(first);
    assert_summaries(&last.active_children, &expected)?;
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    drop(last);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );

    host.runtime_db().transaction(|tx| {
        tx.execute(
            "UPDATE agent_states SET payload_json='{' WHERE agent_id=?1",
            [&identity.agent_id],
        )?;
        Ok(())
    })?;
    let expected_error = borrowed_summaries(&host, &parent).await.unwrap_err();
    let actual_error = host.child_agent_summaries(&parent).await.unwrap_err();
    // Cold qualification is transactional; preserve its rollback evidence as
    // well as the exact canonical decoder chain underneath that typed context.
    let failure = actual_error
        .downcast_ref::<crate::runtime_db::PersistenceFailure>()
        .context("cold Summary omitted transaction failure evidence")?;
    assert_eq!(
        failure.certainty(),
        crate::runtime_db::PersistenceCertainty::DefinitelyNotCommitted
    );
    assert_eq!(
        failure.phase(),
        crate::runtime_db::PersistenceFailurePhase::Callback
    );
    assert_eq!(failure.rollback_error(), None);
    assert_eq!(actual_error.to_string(), expected_error.to_string());
    assert_eq!(
        actual_error
            .chain()
            .skip(1)
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        expected_error
            .chain()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
    );
    assert!(host.local_agent_summary(&parent).await.is_err());
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        baseline
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}
