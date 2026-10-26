use super::*;
use crate::{
    diagnostics::resource_io::ResourceReadMaterial,
    model_catalog::{ModelCapabilityOverride, ModelMetadataConstraintSource},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

fn catalog() -> RuntimeModelCatalog {
    let provider = ProviderId::openai();
    let route = ModelRouteRef::new(
        provider.clone(),
        ProviderEndpointId::default_endpoint(),
        "material-汉🦀",
    );
    let mut catalog = RuntimeModelCatalog {
        default_model: route.clone(),
        ..Default::default()
    };
    catalog.model_overrides.insert(
        route.model_ref(),
        ModelRuntimeOverride {
            display_name: Some("selected-汉🦀".repeat(20)),
            description: Some("selected description".repeat(20)),
            capabilities: Some(ModelCapabilityOverride {
                image_input: Some(true),
                image_generation: Some(true),
                supports_reasoning: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        },
    );
    catalog
        .provider_endpoints
        .push(ResolvedProviderEndpointConfig {
            provider: provider.clone(),
            endpoint: ProviderEndpointId::default_endpoint(),
            runtime_config: ProviderRuntimeConfig {
                id: provider.clone(),
                route_provider: provider,
                route_endpoint: ProviderEndpointId::default_endpoint(),
                transport: ProviderTransportKind::AnthropicMessages,
                base_url: "http://127.0.0.1".into(),
                auth: Default::default(),
                credential: None,
                credential_store_path: None,
                codex_home: None,
                originator: None,
                reasoning_effort: None,
                context_management: Default::default(),
                builtin_web_search: None,
            },
        });
    catalog
}

#[test]
fn policy_only_resolution_preserves_explicit_route_transport_and_alias_contracts() -> Result<()> {
    let mut catalog = catalog();
    let scope = InputEvidenceScope {
        agent_id: "model-policy".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let context = ContextConfig::default();
    for transport in [
        ProviderTransportKind::AnthropicMessages,
        ProviderTransportKind::GeminiGenerateContent,
        ProviderTransportKind::OpenAiResponses,
    ] {
        catalog.provider_endpoints[0].runtime_config.transport = transport;
        for route in [
            catalog.default_model.clone(),
            ModelRouteRef::parse("openai@default/gpt-5.4")?,
            ModelRouteRef::parse("dashscope@token-plan/qwen-3.7")?,
            ModelRouteRef::parse("volcengine@plan/glm-5.2")?,
            ModelRouteRef::parse("openai@unconfigured/material-汉🦀")?,
        ] {
            // Original explicit-route semantics, including transport constraints.
            let expected = catalog
                .resolve_explicit_model_metadata(&context, &route)
                .map(|metadata| metadata.policy)
                .unwrap_or_else(|| catalog.resolved_model_policy_for_route(&context, &route));
            assert_eq!(
                catalog.resolved_model_policy(&context, Some(&route)),
                expected
            );
            let phase =
                ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
            let actual =
                phase.read(|| catalog.try_resolved_model_policy(&context, Some(&route)))?;
            assert_eq!(actual, expected);
            assert_eq!(
                serde_json::to_value(&actual)?,
                serde_json::to_value(&expected)?
            );
            if route == catalog.default_model
                && transport == ProviderTransportKind::AnthropicMessages
            {
                assert!(actual
                    .evidence
                    .constraints
                    .iter()
                    .any(|constraint| constraint.source
                        == ModelMetadataConstraintSource::TransportCapability));
            }
            drop(actual);
            drop(phase);
            assert_eq!(ledger.report()?.observed_bytes, 0);
        }
    }
    Ok(())
}

#[test]
fn policy_material_reuses_exact_original_allowance_and_latches_both_limits() -> Result<()> {
    let catalog = catalog();
    let context = ContextConfig::default();
    let scope = InputEvidenceScope {
        agent_id: "model-policy-budget".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let expected = catalog.resolved_model_policy(&context, None);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    assert_eq!(
        phase.read(|| catalog.try_resolved_model_policy(&context, None))?,
        expected
    );
    let usage = phase.finish()?;
    drop(phase);
    for shortage in [None, Some(true), Some(false)] {
        let phase = ResourceReadMaterial::with_test_limits(
            &ledger,
            &scope,
            usage.0 - u64::from(shortage == Some(true)),
            usage.1 - usize::from(shortage == Some(false)),
        )?;
        let result = phase.read(|| catalog.try_resolved_model_policy(&context, None));
        if shortage.is_none() {
            assert_eq!(result?, expected);
            assert_eq!(phase.finish()?, usage);
            let holder = phase.guard()?;
            let weak = Arc::downgrade(&holder);
            drop(phase);
            assert!(weak.upgrade().is_some());
            drop(holder);
            assert!(weak.upgrade().is_none());
        } else {
            assert_eq!(
                format!("{:#}", result.unwrap_err()),
                "ResourceExhausted: context resource read material"
            );
            let held = ledger.report()?.observed_bytes;
            assert_eq!(
                format!(
                    "{:#}",
                    phase
                        .read(|| catalog.try_resolved_model_policy(&context, None))
                        .unwrap_err()
                ),
                "resourceexhausted: context resource read material",
            );
            assert_eq!(ledger.report()?.observed_bytes, held);
            drop(phase);
        }
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}
