use super::*;

#[tokio::test]
async fn operator_tree_uses_owned_entries_without_activation_and_retains_wire() -> Result<()> {
    let (_home, host) = host()?;
    let parent = register(&host, "owned-tree-parent", "missing-parent")?;
    let child = register(&host, "owned-tree-child", &parent.agent_id)?;
    for identity in [&parent, &child] {
        host.runtime_db()
            .agent_states()
            .initialize_cold_for_fixture(
                &AgentState::new(&identity.agent_id),
                identity.incarnation,
            )?;
    }
    let tree = host.operator_agent_tree().await?;
    let parent_node = tree
        .roots
        .iter()
        .find(|node| node.agent.identity.agent_id == parent.agent_id)
        .context("parent root")?;
    assert_eq!(parent_node.children.len(), 1);
    assert_eq!(
        parent_node.children[0].agent.identity.agent_id,
        child.agent_id
    );
    let expected = serde_json::to_value(&tree)?;
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    let bytes = tree.into_json_bytes()?;
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&bytes)?,
        expected
    );
    let cache = bytes.clone();
    let slice = bytes.slice(1..bytes.len() - 1);
    let held = host.inner.input_evidence_budget.report()?.observed_bytes;
    assert!(held > 0);
    drop(bytes);
    drop(cache);
    assert_eq!(
        host.inner.input_evidence_budget.report()?.observed_bytes,
        held
    );
    drop(slice);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn observer_wire_exact_allowance_and_denial_are_latched_before_buffering() -> Result<()> {
    let (_home, host) = host()?;
    let identity = host.ensure_default_agent_identity()?;
    let scope = InputEvidenceScope {
        agent_id: identity.agent_id,
        identity_incarnation: identity.incarnation,
    };
    let ledger = &host.inner.input_evidence_budget;
    let measure = ResourceReadMaterial::with_test_limits(ledger, &scope, 8 * 1024 * 1024, 8192)?;
    let value = serde_json::json!({ "quoted": "汉🦀 \"{}[],:\\ ".repeat(256) });
    let bytes = crate::host::observer_json_bytes(&value, measure.clone())?;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes)?, value);
    let required = measure.finish()?;
    drop((bytes, measure));
    let exact = ResourceReadMaterial::with_test_limits(ledger, &scope, required.0, required.1)?;
    drop(crate::host::observer_json_bytes(&value, exact.clone())?);
    assert_eq!(exact.finish()?, required);
    drop(exact);
    for (bytes, records) in [(required.0 - 1, required.1), (required.0, required.1 - 1)] {
        let denied = ResourceReadMaterial::with_test_limits(ledger, &scope, bytes, records)?;
        let error = crate::host::observer_json_bytes(&value, denied.clone()).unwrap_err();
        assert!(format!("{error:#}").contains("ResourceExhausted"));
        assert!(denied.finish().is_err());
        assert!(denied.read(|| Ok(())).is_err());
    }
    assert_eq!(ledger.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn cold_summary_selected_domains_remain_pure_after_storage_is_denied() -> Result<()> {
    let (_home, host) = host()?;
    let identity = host.ensure_default_agent_identity()?;
    let state = AgentState::new(&identity.agent_id);
    host.runtime_db()
        .agent_states()
        .initialize_cold_for_fixture(&state, identity.incarnation)?;
    let read = host.acquire_cold_summary(&identity).await?;
    assert_eq!(read.state, state);
    assert!(read.read.is_some());
    assert!(read.projection().active_tasks.is_empty());
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    {
        let connection = host.runtime_db().connection()?;
        connection.execute_batch(
            "ALTER TABLE tasks RENAME TO forbidden_cold_summary_tasks;
             ALTER TABLE wait_conditions RENAME TO forbidden_cold_summary_waits;
             ALTER TABLE briefs RENAME TO forbidden_cold_summary_briefs;",
        )?;
    }
    let observation = read.observability()?;
    assert!(observation.last_result_brief.is_none());
    assert!(observation._read_material.is_some());
    let guard = Arc::downgrade(
        observation
            ._read_material
            .as_ref()
            .context("child material")?,
    );
    drop(read);
    assert!(guard.upgrade().is_some());
    drop(observation);
    assert!(guard.upgrade().is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    // Restore test schema for ordinary shutdown; pure conversion above never
    // obtained a database handle.
    host.runtime_db().connection()?.execute_batch(
        "ALTER TABLE forbidden_cold_summary_tasks RENAME TO tasks;
         ALTER TABLE forbidden_cold_summary_waits RENAME TO wait_conditions;
         ALTER TABLE forbidden_cold_summary_briefs RENAME TO briefs;",
    )?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn no_state_summary_rejects_mixed_domain_before_decoding_giant_raw_body() -> Result<()> {
    let (_home, host) = host()?;
    let identity = host.ensure_default_agent_identity()?;
    let connection = host.runtime_db().connection()?;
    connection.execute(
        "INSERT INTO tasks(task_id,owner_agent_id,kind,status,revision,created_at,updated_at,payload_json)
         VALUES('unproved-no-state-task',?1,'command_task','running',1,'2026-10-22T00:00:00Z','2026-10-22T00:00:00Z',?2)",
        rusqlite::params![identity.agent_id, "not-json".repeat(128 * 1024)],
    )?;
    drop(connection);
    let error = match host.acquire_cold_summary(&identity).await {
        Ok(_) => anyhow::bail!("mixed NoState acquired Summary"),
        Err(error) => error,
    };
    assert!(
        format!("{error:#}").contains("NoState Summary has unproved tasks members"),
        "{error:#}"
    );
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.runtime_db().connection()?.execute(
        "DELETE FROM tasks WHERE task_id='unproved-no-state-task'",
        [],
    )?;
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn observer_inventory_and_hint_wire_keep_material_without_body_authority() -> Result<()> {
    let (_home, host) = host()?;
    let snapshot = host.agent_roster_snapshot()?;
    let material = snapshot.acquisition.clone();
    let guard = Arc::downgrade(&material.guard()?);
    assert!(material.matches_identity(&snapshot.runtime_id, 1));
    let value = material.read(|| {
        crate::diagnostics::resource_io::admit_cloned_value(&snapshot.runtime_id)?;
        Ok(serde_json::json!({ "runtime_id": snapshot.runtime_id }))
    })?;
    drop(snapshot);
    let bytes = crate::host::observer_json_bytes(&value, material)?;
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&bytes)?, value);
    let cache = bytes.clone();
    let slice = bytes.slice(0..1);
    drop(bytes);
    drop(cache);
    assert!(guard.upgrade().is_some());
    drop(slice);
    assert!(guard.upgrade().is_none());
    let hint =
        crate::host::observer_hint_bytes("quoted \"汉🦀", host.observer_inventory_material()?)?;
    assert_eq!(
        std::str::from_utf8(&hint)?,
        "event: agent_roster_hint\ndata: {\"agent_id\":\"quoted \\\"汉🦀\"}\n\n"
    );
    let slice = hint.slice(1..);
    drop(hint);
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > 0);
    drop(slice);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn global_hint_http_keeps_raw_event_body_out_of_metadata_stream() -> Result<()> {
    use axum::{extract::State, response::IntoResponse};
    use futures_util::StreamExt;
    let (_home, host) = host()?;
    let identity = host.ensure_default_agent_identity()?;
    let response = crate::http::global_events_stream(
        State(Arc::new(crate::http::AppState::for_tcp(host.clone()))),
        axum::http::HeaderMap::new(),
    )
    .await
    .map_err(|(status, body)| anyhow::anyhow!("{status}: {:?}", body.0))?
    .into_response();
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "text/event-stream"
    );
    assert_eq!(
        response.headers()[axum::http::header::CACHE_CONTROL],
        "no-cache"
    );
    let storage = host.agent_storage(&identity.agent_id)?;
    storage.append_event(&crate::types::AuditEvent::legacy(
        "raw-hint-only",
        serde_json::json!({ "private_body": "must never be in global hint" }),
    ))?;
    let mut body = response.into_body().into_data_stream();
    let bytes = tokio::time::timeout(Duration::from_secs(2), body.next())
        .await?
        .context("global hint stream ended")??;
    let text = std::str::from_utf8(&bytes)?;
    assert!(text.starts_with("event: agent_roster_hint\ndata: "));
    let hint: serde_json::Value = serde_json::from_str(
        text.split_once("\ndata: ")
            .context("hint framing")?
            .1
            .trim_end(),
    )?;
    assert_eq!(hint, serde_json::json!({ "agent_id": identity.agent_id }));
    assert!(!text.contains("private_body"));
    let slice = bytes.slice(1..);
    drop(bytes);
    drop(body);
    assert!(host.inner.input_evidence_budget.report()?.observed_bytes > 0);
    drop(slice);
    assert_eq!(host.inner.input_evidence_budget.report()?.observed_bytes, 0);
    assert!(host.inner.runtimes.read().await.agents.is_empty());
    host.shutdown().await?;
    Ok(())
}
