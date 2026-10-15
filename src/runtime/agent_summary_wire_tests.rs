use super::*;
use crate::runtime::tests::support::{context_config, tempdir};

async fn runtime(home: &std::path::Path) -> Result<RuntimeHandle> {
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "summary-wire-owner",
        home.into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    drop(runtime.agent_summary().await?);
    Ok(runtime)
}

#[tokio::test]
async fn actual_full_summary_http_body_retains_source_and_encoded_guard_until_last_slice(
) -> Result<()> {
    use futures_util::StreamExt;
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    runtime
        .default_external_trigger(CallbackDeliveryMode::WakeHint)
        .await?;
    drop(runtime.agent_summary().await?);
    let _pause = runtime.pause_resource_observer_for_test().await;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    let read = runtime.agent_summary().await?;
    let expected = serde_json::to_vec(&*read)?;
    let source = Arc::downgrade(&read.material._owned[0]);
    let model_source = Arc::downgrade(
        read.material
            ._model
            .as_ref()
            .context("loaded summary omitted selected model material")?,
    );
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let root_holders = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO summary_wire_forbidden_states;
         ALTER TABLE audit_events RENAME TO summary_wire_forbidden_events;
         ALTER TABLE tasks RENAME TO summary_wire_forbidden_tasks;
         ALTER TABLE external_triggers RENAME TO summary_wire_forbidden_triggers;",
    )?;
    // This is the same helper used by the real Full status/get_agent handler.
    let response = crate::http::traced_summary_json(
        "/agents/{agent_id}/status",
        std::time::Instant::now(),
        read,
    )
    .map_err(|(status, body)| anyhow!("HTTP summary {status}: {}", body.0))?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "application/json"
    );
    assert!(
        source.upgrade().is_some(),
        "handler dropped original source material"
    );
    assert_eq!(
        Arc::strong_count(&root),
        root_holders,
        "encoded body retained a Root"
    );
    let mut body = response.into_body().into_data_stream();
    let chunk = body.next().await.context("actual response body empty")??;
    assert_eq!(chunk.as_ref(), expected.as_slice());
    assert!(body.next().await.is_none());
    drop(body);
    let clone = chunk.clone();
    let last = clone.slice(0..1);
    drop((clone, chunk));
    assert!(
        source.upgrade().is_some(),
        "last Bytes slice did not own original carrier"
    );
    assert!(ledger.report()?.observed_bytes > baseline);
    assert!(model_source.upgrade().is_some());
    drop(last);
    assert!(model_source.upgrade().is_none());
    assert!(source.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}

#[tokio::test]
async fn actual_summary_encoder_borrows_source_at_exact_byte_and_node_limits_without_double_register(
) -> Result<()> {
    let home = tempdir()?;
    let runtime = runtime(home.path()).await?;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let ledger = &runtime.inner.input_evidence_budget;
    let baseline = ledger.report()?.observed_bytes;
    for kind in ["byte", "nodes", "exact", "foreign", "successor"] {
        let mut read = runtime.agent_summary().await?;
        let source = Arc::downgrade(&read.material._owned[0]);
        let plan = read.json_wire_plan()?;
        let own = plan.own;
        assert!(own.0 < plan.total.0 && own.1 < plan.total.1);
        let exact = (usize::try_from(plan.total.0)?, plan.total.1);
        if kind == "foreign" || kind == "successor" {
            let other = InputEvidenceBudgetLedger::new(Default::default())?;
            if kind == "foreign" {
                read.material.ledger = other.clone();
            } else {
                read.material.scope.identity_incarnation += 1;
            }
            let error = read
                .into_json_wire_with_limits(exact)
                .err()
                .context("foreign wire source was accepted")?;
            assert!(
                matches!(
                    error.downcast_ref::<AgentSummaryWireError>(),
                    Some(AgentSummaryWireError::ForeignMaterial)
                ),
                "{error:#}"
            );
            assert_eq!(other.report()?.observed_bytes, 0);
        } else if kind == "exact" {
            let before = ledger.report()?.observed_bytes;
            let wire = read.into_json_wire_with_limits(exact)?;
            assert_eq!(wire.bytes.len(), plan.encoded_len);
            assert_eq!(
                ledger.report()?.observed_bytes - before,
                own.0,
                "original source allocations were registered twice"
            );
            let encoded = Arc::downgrade(&wire._encoded);
            let bytes = axum::body::Bytes::from_owner(wire);
            let last = bytes.clone();
            drop(bytes);
            assert!(encoded.upgrade().is_some() && source.upgrade().is_some());
            drop(last);
            assert!(encoded.upgrade().is_none());
        } else {
            let limits = if kind == "byte" {
                (exact.0 - 1, exact.1)
            } else {
                (exact.0, exact.1 - 1)
            };
            let error = read
                .into_json_wire_with_limits(limits)
                .err()
                .context("summary source was given an independent full encoder allowance")?;
            assert!(
                matches!(
                    error.downcast_ref::<AgentSummaryWireError>(),
                    Some(AgentSummaryWireError::ResourceExhausted)
                ),
                "{error:#}"
            );
        }
        assert!(source.upgrade().is_none());
        assert_eq!(ledger.report()?.observed_bytes, baseline, "kind {kind}");
    }
    Ok(())
}

#[tokio::test]
async fn actual_cold_full_status_route_keeps_wire_material_without_runtime_or_state_activation(
) -> Result<()> {
    use crate::{config::AppConfig, host::RuntimeHost, provider::StubProvider};
    use axum::{body::Body, http::Request, routing::get, Router};
    use futures_util::StreamExt;
    use tower::ServiceExt;
    let home = tempfile::tempdir()?;
    std::fs::write(
        home.path().join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    let mut config = {
        let _lock = crate::test_env::lock_env();
        AppConfig::load_with_home(Some(home.path().into()))?
    };
    config.user_home_dir = Some(home.path().into());
    config.control_auth_mode = crate::config::ControlAuthMode::Disabled;
    let host = RuntimeHost::new_with_provider(config, Arc::new(StubProvider::new("unused")))?;
    let identity = host.ensure_default_agent_identity()?;
    assert!(host
        .try_get_loaded_runtime(&identity.agent_id)
        .await
        .is_none());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    let read = host
        .local_agent_summary(&identity.agent_id)
        .await
        .map_err(|error| anyhow!("{error:?}"))?;
    let ledger = read.material.ledger.clone();
    let plan = read.json_wire_plan()?;
    let wire = read.into_json_wire()?;
    assert_eq!(wire._encoded.owned_material_bytes()?, plan.own.0);
    assert_eq!(wire.bytes.len(), plan.encoded_len);
    drop(wire);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    let state = Arc::new(crate::http::AppState::for_unix(host.clone()));
    let app = Router::new()
        .route("/agents/{agent_id}/status", get(crate::http::status))
        .with_state(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/agents/{}/status?detail=full", identity.agent_id))
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(
        response.headers()[axum::http::header::CONTENT_TYPE],
        "application/json"
    );
    assert!(
        ledger.report()?.observed_bytes > 0,
        "actual handler returned naked bytes and dropped cold material"
    );
    let mut body = response.into_body().into_data_stream();
    let chunk = body.next().await.context("cold HTTP body empty")??;
    let json: serde_json::Value = serde_json::from_slice(&chunk)?;
    assert_eq!(json["identity"]["agent_id"], identity.agent_id);
    assert_eq!(json["identity"]["incarnation"], identity.incarnation);
    assert_eq!(json["agent"]["status"], "stopped");
    let last = chunk.slice(0..1);
    drop((chunk, body, json));
    assert!(ledger.report()?.observed_bytes > 0);
    assert!(host
        .try_get_loaded_runtime(&identity.agent_id)
        .await
        .is_none());
    assert!(host
        .runtime_db()
        .agent_states()
        .latest(&identity.agent_id)?
        .is_none());
    drop(last);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    host.shutdown().await?;
    Ok(())
}
