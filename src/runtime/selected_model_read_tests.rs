use super::*;
use crate::config::{
    AnthropicContextManagementConfig, ModelRef, ProviderAuthConfig, ProviderRuntimeConfig,
    ProviderTransportKind, ResolvedProviderEndpointConfig,
};
use crate::model_catalog::ModelRuntimeOverride;
use crate::runtime::tests::support::{context_config, tempdir};

async fn fixture() -> Result<(tempfile::TempDir, RuntimeHandle)> {
    let home = tempdir()?;
    let runtime = RuntimeHandle::new_isolated_resource_fixture(
        "selected-model-read",
        home.path().into(),
        context_config(),
    )?;
    runtime.refresh_context_resources().await?;
    Ok((home, runtime))
}
fn endpoint(route: &ModelRouteRef) -> ResolvedProviderEndpointConfig {
    ResolvedProviderEndpointConfig {
        provider: route.provider.clone(),
        endpoint: route.endpoint.clone(),
        runtime_config: ProviderRuntimeConfig {
            id: route.provider.clone(),
            route_provider: route.provider.clone(),
            route_endpoint: route.endpoint.clone(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            auth: ProviderAuthConfig::default(),
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: AnthropicContextManagementConfig::default(),
            builtin_web_search: None,
        },
    }
}
fn capture(
    catalog: &RuntimeModelCatalog,
    read: &super::super::state_mutation::PreparedSchedulerRead,
    ledger: &InputEvidenceBudgetLedger,
    limits: (usize, usize),
    prior: (u64, usize),
) -> Result<(AgentModelState, SelectedModelReadMaterial)> {
    capture_selected_model_state(
        catalog,
        &context_config(),
        &read.state,
        &read.work.scope,
        ledger,
        &read._owned_budget,
        limits,
        prior,
    )
}
#[tokio::test]
async fn actual_selected_model_read_preserves_original_policy_chain_and_ignores_large_unselected_config(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let read = runtime.capture_scheduler_projection().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let mut catalog = RuntimeModelCatalog::default();
    catalog
        .provider_endpoints
        .push(endpoint(&catalog.default_model));
    let fallback = ModelRouteRef::parse("anthropic@default/claude-sonnet-4-6")?;
    catalog.fallback_models = vec![
        catalog.default_model.clone(),
        fallback.clone(),
        fallback.clone(),
    ];
    let expected =
        super::super::agent_model_state_for_catalog(&catalog, &context_config(), &read.state);
    let _pause = runtime.pause_resource_observer_for_test().await;
    let root = runtime.inner.mutation_owner.capture()?.unwrap();
    let count = Arc::strong_count(&root);
    runtime.runtime_db().connection()?.execute_batch(
        "ALTER TABLE agent_states RENAME TO model_read_forbidden_states;
         ALTER TABLE work_items RENAME TO model_read_forbidden_work;
         ALTER TABLE tasks RENAME TO model_read_forbidden_tasks;
         ALTER TABLE audit_events RENAME TO model_read_forbidden_events;",
    )?;
    let baseline = ledger.report()?.observed_bytes;
    let (model, material) = capture(
        &catalog,
        &read,
        ledger,
        (usize::MAX, usize::MAX),
        read.material_estimate()?,
    )?;
    assert_eq!(
        serde_json::to_value(&model)?,
        serde_json::to_value(&expected)?
    );
    assert_eq!(model.effective_fallback_models, vec![fallback]);
    let own = material.material_estimate();
    material.validate_accounting(&read.work.scope, ledger)?;
    assert_eq!(ledger.report()?.observed_bytes - baseline, own.0);
    let weak = Arc::downgrade(&material.guard);
    drop((model, material));
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    // These entries are never selected by the actual resolver and must not be
    // cloned, hashed, serialized or conservatively charged as a whole catalog.
    catalog.model_overrides.insert(
        ModelRef::new(catalog.default_model.provider.clone(), "unselected-large"),
        ModelRuntimeOverride {
            description: Some("x".repeat(8 * 1024 * 1024)),
            ..Default::default()
        },
    );
    let mut unused = endpoint(&ModelRouteRef::parse("anthropic@default/unused")?);
    unused.runtime_config.originator = Some("y".repeat(8 * 1024 * 1024));
    catalog.provider_endpoints.push(unused);
    let (again, material) = capture(
        &catalog,
        &read,
        ledger,
        (usize::MAX, usize::MAX),
        read.material_estimate()?,
    )?;
    assert_eq!(material.material_estimate(), own);
    assert_eq!(
        serde_json::to_value(&again)?,
        serde_json::to_value(&expected)?
    );
    assert_eq!(
        Arc::strong_count(&root),
        count,
        "model result retained a Root"
    );
    drop((again, material));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
#[tokio::test]
async fn actual_selected_model_material_borrows_prior_at_exact_byte_node_limits_and_rejects_foreign_ledger(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let read = runtime.capture_scheduler_projection().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let held = vec![0u8; 128 * 1024];
    let prior_guard = ledger.register_owned_bytes(
        &read.work.scope,
        InputMemoryCategory::Pins,
        OwnedBytesEstimate {
            payload_bytes: held.len() as u64,
            metadata_estimate_bytes: 1024,
        },
    )?;
    let source = read.material_estimate()?;
    let prior = (source.0 + prior_guard.owned_material_bytes()?, source.1 + 4);
    let catalog = RuntimeModelCatalog::default();
    let (model, measured) = capture(&catalog, &read, ledger, (usize::MAX, usize::MAX), prior)?;
    let own = measured.material_estimate();
    drop((model, measured));
    let baseline = ledger.report()?.observed_bytes;
    let exact = (usize::try_from(prior.0 + own.0)?, prior.1 + own.1);
    for limits in [(exact.0 - 1, exact.1), (exact.0, exact.1 - 1)] {
        let error = capture(&catalog, &read, ledger, limits, prior)
            .err()
            .context("insufficient joint material was accepted")?;
        assert!(matches!(
            error.downcast_ref::<SelectedModelReadError>(),
            Some(SelectedModelReadError::ResourceExhausted)
        ));
        assert_eq!(ledger.report()?.observed_bytes, baseline);
    }
    let (model, material) = capture(&catalog, &read, ledger, exact, prior)?;
    assert_eq!(material.material_estimate(), own);
    assert_eq!(
        ledger.report()?.observed_bytes - baseline,
        own.0,
        "caller prior was registered twice"
    );
    let weak = Arc::downgrade(&material.guard);
    let last = Arc::new(material);
    let clone = last.clone();
    drop((last, model));
    assert!(weak.upgrade().is_some());
    drop(clone);
    assert!(weak.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    let foreign = InputEvidenceBudgetLedger::new(Default::default())?;
    let error = capture(&catalog, &read, &foreign, exact, prior)
        .err()
        .context("foreign ledger accepted")?;
    assert!(matches!(
        error.downcast_ref::<SelectedModelReadError>(),
        Some(SelectedModelReadError::ForeignMaterial)
    ));
    assert_eq!(foreign.report()?.observed_bytes, 0);
    drop((held, prior_guard, read));
    Ok(())
}
#[tokio::test]
async fn actual_selected_policy_and_endpoint_large_fields_reject_before_their_original_copy(
) -> Result<()> {
    let (_home, runtime) = fixture().await?;
    let read = runtime.capture_scheduler_projection().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let prior = read.material_estimate()?;
    for selected in ["policy", "endpoint"] {
        let mut catalog = RuntimeModelCatalog::default();
        if selected == "policy" {
            catalog.model_overrides.insert(
                catalog.default_model.model_ref(),
                ModelRuntimeOverride {
                    description: Some("selected-description".repeat(128 * 1024)),
                    ..Default::default()
                },
            );
        } else {
            let mut actual = endpoint(&catalog.default_model);
            actual.runtime_config.context_management.betas =
                vec!["selected-beta".repeat(128 * 1024)];
            catalog.provider_endpoints.push(actual);
        }
        let baseline = ledger.report()?.observed_bytes;
        let limits = (usize::try_from(prior.0)? + 512 * 1024, prior.1 + 16_384);
        let error = capture(&catalog, &read, ledger, limits, prior)
            .err()
            .context("large selected source was copied without admission")?;
        assert!(matches!(
            error.downcast_ref::<SelectedModelReadError>(),
            Some(SelectedModelReadError::ResourceExhausted)
        ));
        assert_eq!(
            ledger.report()?.observed_bytes,
            baseline,
            "failed resolver retained partial material"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[tokio::test]
async fn actual_selected_endpoint_opaque_non_utf8_paths_preserve_original_model_view_and_copy_cost(
) -> Result<()> {
    use std::os::unix::ffi::OsStringExt;
    let (_home, runtime) = fixture().await?;
    let read = runtime.capture_scheduler_projection().await?;
    let ledger = &runtime.inner.input_evidence_budget;
    let _pause = runtime.pause_resource_observer_for_test().await;
    let mut catalog = RuntimeModelCatalog::default();
    let mut actual = endpoint(&catalog.default_model);
    actual.runtime_config.credential_store_path =
        Some(std::ffi::OsString::from_vec(vec![b'/', 0xff, b'c']).into());
    actual.runtime_config.codex_home =
        Some(std::ffi::OsString::from_vec(vec![b'/', 0xfe, b'h']).into());
    catalog.provider_endpoints.push(actual);
    let expected =
        super::super::agent_model_state_for_catalog(&catalog, &context_config(), &read.state);
    let baseline = ledger.report()?.observed_bytes;
    let (model, material) = capture(
        &catalog,
        &read,
        ledger,
        (usize::MAX, usize::MAX),
        read.material_estimate()?,
    )?;
    assert_eq!(
        serde_json::to_value(&model)?,
        serde_json::to_value(&expected)?
    );
    let with_paths = material.material_estimate();
    drop((model, material));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    catalog.provider_endpoints[0]
        .runtime_config
        .credential_store_path = None;
    catalog.provider_endpoints[0].runtime_config.codex_home = None;
    let (model, material) = capture(
        &catalog,
        &read,
        ledger,
        (usize::MAX, usize::MAX),
        read.material_estimate()?,
    )?;
    let without_paths = material.material_estimate();
    assert_eq!(with_paths.0 - without_paths.0, 6 + 2 * 256);
    assert_eq!(with_paths.1 - without_paths.1, 2);
    drop((model, material));
    assert_eq!(ledger.report()?.observed_bytes, baseline);
    Ok(())
}
