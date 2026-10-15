use super::*;
use crate::runtime::tests::support::{host_backed_test_runtime, persist_task_transition};
use crate::types::{AgentLifecycleDisposition, ChildAgentWorkspaceMode, TaskRecoverySpec};
const LIMITS: (usize, usize) = (64 * 1024 * 1024, 65536);

async fn add_child(
    host: &RuntimeHost,
    parent: &RuntimeHandle,
    loaded: bool,
) -> Result<AgentIdentityRecord> {
    let now = Utc::now();
    let parent_id = parent.instance_agent_id();
    let task = TaskRecord {
        id: crate::ids::task_id(),
        agent_id: parent_id.into(),
        kind: TaskKind::ChildAgentTask,
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        parent_message_id: None,
        work_item_id: None,
        summary: Some("original child observation".into()),
        detail: Some(json!({"workspace_mode":"inherit"})),
        recovery: Some(TaskRecoverySpec::ChildAgentTask {
            summary: "original child observation".into(),
            prompt: "actual accepted child".into(),
            authority_class: AuthorityClass::OperatorInstruction,
            workspace_mode: ChildAgentWorkspaceMode::Inherit,
            lifecycle_disposition: AgentLifecycleDisposition::Retain,
        }),
    };
    persist_task_transition(parent, &task, "child_summary_parent_ack").await?;
    let identity = host
        .create_child_identity(parent_id, &task, None, &host.agent_data_dir(parent_id))
        .await?;
    if loaded {
        let child = host.get_or_create_agent(&identity.agent_id).await?;
        let active = TaskRecord {
            id: crate::ids::task_id(),
            agent_id: identity.agent_id.clone(),
            kind: TaskKind::AgentMessageWait,
            status: TaskStatus::Running,
            created_at: now,
            updated_at: now,
            parent_message_id: None,
            work_item_id: None,
            summary: Some("actual child blocking wait".into()),
            detail: Some(json!({"target_agent_id":parent_id,"business_completion":false})),
            recovery: Some(TaskRecoverySpec::AgentMessageWait {
                summary: "actual child blocking wait".into(),
                message: "original request".into(),
                target_agent_id: parent_id.into(),
                authority_class: AuthorityClass::RuntimeInstruction,
                delivery_id: None,
                after_delivery_rowid: None,
            }),
        };
        persist_task_transition(&child, &active, "child_summary_active_task_ack").await?;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if child.agent_state().await?.status == AgentStatus::Asleep {
                    break Ok::<(), anyhow::Error>(());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .context("original child did not reach actual idle sleep")??;
        drop(host.child_agent_summaries(parent_id).await?);
        child.refresh_context_resources().await?;
        child.quiesce_child_summary_observer_for_test().await?;
    }
    Ok(identity)
}
fn restore(db: &RuntimeDb) -> Result<()> {
    db.connection()?.execute_batch(
        "ALTER TABLE child_host_forbidden_tasks RENAME TO tasks;
         ALTER TABLE child_host_forbidden_state RENAME TO agent_states;
         ALTER TABLE child_host_forbidden_work RENAME TO work_items;
         ALTER TABLE child_host_forbidden_identity RENAME TO agent_identities;",
    )?;
    Ok(())
}
fn deny(db: &RuntimeDb) -> Result<()> {
    db.connection()?.execute_batch(
        "ALTER TABLE tasks RENAME TO child_host_forbidden_tasks;
         ALTER TABLE agent_states RENAME TO child_host_forbidden_state;
         ALTER TABLE work_items RENAME TO child_host_forbidden_work;
         ALTER TABLE agent_identities RENAME TO child_host_forbidden_identity;",
    )?;
    Ok(())
}
#[tokio::test]
async fn actual_loaded_child_host_and_parent_wires_keep_original_sources_until_last_encoded_holder(
) -> Result<()> {
    let (_home, host, parent) = host_backed_test_runtime().await;
    let identity = add_child(&host, &parent, true).await?;
    let expected = host
        .child_agent_summaries(parent.instance_agent_id())
        .await?;
    assert_eq!(expected.len(), 1);
    assert_eq!(expected[0].identity.agent_id, identity.agent_id);
    assert_eq!(expected[0].active_task_count, 1);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if parent.agent_state().await?.status == AgentStatus::Asleep {
                break Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("original parent did not reach actual idle sleep")??;
    parent.refresh_context_resources().await?;
    parent.quiesce_child_summary_observer_for_test().await?;
    let ledger = &host.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    deny(host.runtime_db())?;
    let children = host
        .try_loaded_owned_child_summaries(parent.instance_agent_id(), ledger, LIMITS, (0, 0))
        .await?
        .context("original child became cold")?;
    assert_eq!(children.summaries, expected);
    let weak = Arc::downgrade(&children.material.selector);
    let (summaries, material) = children.into_parts();
    assert!(material.material_estimate()?.0 > 0);
    drop(summaries);
    assert!(weak.upgrade().is_some());
    drop(material);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let mut false_host_view = host
        .try_loaded_owned_child_summaries(parent.instance_agent_id(), ledger, LIMITS, (0, 0))
        .await?
        .unwrap();
    let scope = crate::runtime::resident_input_evidence::InputEvidenceScope {
        agent_id: parent.instance_agent_id().into(),
        identity_incarnation: parent.instance_incarnation(),
    };
    false_host_view.material.selector = ledger.register_owned_bytes(
        &scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: false_host_view.material.selector_cost.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let error = false_host_view
        .material
        .validate_accounting(ledger)
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<ChildAgentSummariesReadError>(),
            Some(ChildAgentSummariesReadError::ForeignMaterial)
        ),
        "agent-scope guard cannot stand in for original Host selector"
    );
    drop(false_host_view);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let compact = parent.lightweight_agent_state_projection().await?;
    assert_eq!(compact.active_children, expected);
    let weak = Arc::downgrade(
        compact
            ._ordinary_material
            .as_ref()
            .unwrap()
            .children_material_for_test()
            .unwrap()
            .selector_guard_for_test(),
    );
    let body = axum::body::Bytes::from_owner(compact.into_compact_json_wire()?);
    assert!(serde_json::from_slice::<Value>(&body)?["active_children"].is_array());
    let last = body.slice(..1);
    drop(body);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let full = parent.agent_summary().await?;
    assert_eq!(full.active_children, expected);
    let weak = Arc::downgrade(
        full.children_material_for_test()
            .unwrap()
            .selector_guard_for_test(),
    );
    let body = axum::body::Bytes::from_owner(full.into_json_wire()?);
    let wire: Value = serde_json::from_slice(&body)?;
    assert_eq!(wire["active_children"], serde_json::to_value(&expected)?);
    drop(wire);
    let last = body.slice(..1);
    drop(body);
    assert!(weak.upgrade().is_some());
    drop(last);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    restore(host.runtime_db())?;
    host.shutdown().await?;
    Ok(())
}
#[tokio::test]
async fn original_host_child_selector_and_prior_reject_before_copy_and_cold_is_not_empty_or_a_loaded_error_fallback(
) -> Result<()> {
    let (_home, host, parent) = host_backed_test_runtime().await;
    let identity = add_child(&host, &parent, true).await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if parent.agent_state().await?.status == AgentStatus::Asleep {
                break Ok::<(), anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("original parent did not reach actual idle sleep")??;
    parent.refresh_context_resources().await?;
    parent.quiesce_child_summary_observer_for_test().await?;
    let ledger = &host.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let selector =
        host.select_child_summary_addresses(parent.instance_agent_id(), ledger, LIMITS, (0, 0))?;
    let selector_cost = selector.cost;
    assert_eq!(selector.identities.len(), 1);
    assert_eq!(selector.identities[0].agent_id, identity.agent_id);
    drop(selector);
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    for limit in [
        (usize::try_from(selector_cost.0)? - 1, selector_cost.1),
        (usize::try_from(selector_cost.0)?, selector_cost.1 - 1),
    ] {
        let error = host
            .select_child_summary_addresses(parent.instance_agent_id(), ledger, limit, (0, 0))
            .err()
            .context("selector insufficient cap accepted")?;
        assert!(matches!(
            error.downcast_ref::<ChildAgentSummariesReadError>(),
            Some(ChildAgentSummariesReadError::ResourceExhausted)
        ));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let exact = host.select_child_summary_addresses(
        parent.instance_agent_id(),
        ledger,
        (usize::try_from(selector_cost.0)?, selector_cost.1),
        (0, 0),
    )?;
    drop(exact);
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let error = host
        .select_child_summary_addresses(parent.instance_agent_id(), &foreign, LIMITS, (0, 0))
        .err()
        .context("foreign ledger accepted")?;
    assert!(matches!(
        error.downcast_ref::<ChildAgentSummariesReadError>(),
        Some(ChildAgentSummariesReadError::ForeignMaterial)
    ));
    assert_eq!(foreign.report()?.observed_bytes, 0);
    let prior = (128 * 1024u64, 32usize);
    let prior_guard = ledger.register_host_owned_bytes(
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: prior.0,
            metadata_estimate_bytes: 0,
        },
    )?;
    let prior_body = vec![0u8; usize::try_from(prior.0)?];
    let error = host
        .select_child_summary_addresses(
            parent.instance_agent_id(),
            ledger,
            (
                usize::try_from(prior.0 + selector_cost.0 - 1)?,
                prior.1 + selector_cost.1,
            ),
            prior,
        )
        .err()
        .context("actual held prior was omitted")?;
    assert!(matches!(
        error.downcast_ref::<ChildAgentSummariesReadError>(),
        Some(ChildAgentSummariesReadError::ResourceExhausted)
    ));
    drop((prior_body, prior_guard));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    // Explicit adversarial cache seam, not a sanctioned Host Runtime factory:
    // the originally selected address must not lend material to another ledger.
    let foreign_home = crate::runtime::tests::support::tempdir()?;
    let foreign_runtime = RuntimeHandle::child_summary_foreign_ledger_fixture(
        &identity.agent_id,
        foreign_home.path().into(),
    )?;
    foreign_runtime
        .quiesce_child_summary_observer_for_test()
        .await?;
    assert!(!foreign_runtime.child_summary_uses_ledger(ledger));
    let original_runtime = {
        let mut registry = host.inner.runtimes.write().await;
        std::mem::replace(
            &mut registry
                .agents
                .get_mut(&identity.agent_id)
                .context("actual child cache entry absent")?
                .runtime,
            foreign_runtime,
        )
    };
    let actual = host
        .try_loaded_owned_child_summaries(parent.instance_agent_id(), ledger, LIMITS, (0, 0))
        .await;
    {
        let mut registry = host.inner.runtimes.write().await;
        registry.agents.get_mut(&identity.agent_id).unwrap().runtime = original_runtime;
    }
    let error = actual
        .err()
        .context("foreign child ledger was lent original Host prior")?;
    assert!(matches!(
        error.downcast_ref::<ChildAgentSummariesReadError>(),
        Some(ChildAgentSummariesReadError::ForeignMaterial)
    ));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let child = host
        .try_get_loaded_runtime(&identity.agent_id)
        .await
        .context("actual child absent")?;
    let original = child.replace_task_domain_for_reader_test(None).await?;
    let error = host
        .try_loaded_owned_child_summaries(parent.instance_agent_id(), ledger, LIMITS, (0, 0))
        .await
        .err()
        .context("loaded unprepared Task silently fell back")?;
    assert!(matches!(
        error.downcast_ref::<crate::runtime::resident_task::TaskReadError>(),
        Some(crate::runtime::resident_task::TaskReadError::IncompleteActive)
    ));
    child.replace_task_domain_for_reader_test(original).await?;
    let cold = add_child(&host, &parent, false).await?;
    assert!(host.try_get_loaded_runtime(&cold.agent_id).await.is_none());
    let actual_baseline = ledger.report()?.observed_bytes;
    assert!(host
        .try_loaded_owned_child_summaries(parent.instance_agent_id(), ledger, LIMITS, (0, 0))
        .await?
        .is_none());
    assert_eq!(ledger.report()?.observed_bytes, actual_baseline);
    assert!(
        host.try_get_loaded_runtime(&cold.agent_id).await.is_none(),
        "cold was activated by inspection"
    );
    host.shutdown().await?;
    Ok(())
}
