use super::*;
use crate::{
    config::ServiceTier,
    diagnostics::resource_io::ResourceReadMaterial,
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};

fn catalog() -> Result<RuntimeModelCatalog> {
    let primary = ModelRouteRef::parse("openai@default/gpt-5.4")?;
    let fallback = ModelRouteRef::parse("deepseek@responses/deepseek-v4-pro")?;
    Ok(RuntimeModelCatalog {
        default_model: primary.clone(),
        fallback_models: vec![fallback.clone(), primary, fallback],
        ..Default::default()
    })
}

fn original_turn_model(
    catalog: &RuntimeModelCatalog,
    context: &ContextConfig,
    state: &AgentState,
    fallback: Option<&ModelRouteRef>,
) -> AgentModelState {
    let mut model = agent_model_state_for_catalog(catalog, context, state);
    if let Some(fallback) = fallback {
        model.effective_service_tier = if model.effective_model == *fallback {
            state
                .model_override_service_tier
                .or_else(|| catalog.route_service_tier(fallback))
        } else {
            catalog.route_service_tier(fallback)
        };
        model.active_model = Some(fallback.clone());
        model.fallback_active = model.effective_model != *fallback;
        model.resolved_policy = catalog.resolved_model_policy(context, Some(fallback));
        // Preserve the original owning-chain algorithm as an independent oracle.
        let chain = catalog.provider_chain(state.model_override.as_ref());
        model.effective_fallback_models = chain
            .iter()
            .position(|route| route == fallback)
            .map(|index| chain[index..].iter().skip(1).cloned().collect())
            .unwrap_or_default();
    }
    model
}

#[test]
fn turn_model_selection_matches_original_fallback_and_last_active_contract() -> Result<()> {
    let mut catalog = catalog()?;
    let context = ContextConfig::default();
    let mut state = AgentState::new("turn-model");
    state.model_override_reasoning_effort = Some("high-汉🦀".into());
    state.model_override_service_tier = Some(serde_json::from_str::<ServiceTier>("\"priority\"")?);
    let fallback = catalog.fallback_models[0].clone();
    let foreign = ModelRouteRef::parse("openai@unconfigured/fallback-汉🦀")?;
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let scope = InputEvidenceScope {
        agent_id: state.id.clone(),
        identity_incarnation: 1,
    };
    for disabled in [false, true] {
        catalog.disable_provider_fallback = disabled;
        for override_model in [None, Some(foreign.clone())] {
            state.model_override = override_model;
            state.last_requested_model =
                Some(catalog.effective_model(state.model_override.as_ref()));
            state.last_active_model = Some(fallback.clone());
            for active in [
                None,
                Some(&catalog.default_model),
                Some(&fallback),
                Some(&foreign),
            ] {
                let expected = original_turn_model(&catalog, &context, &state, active);
                let phase =
                    ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
                let prepared = prepare_agent_model_state_for_turn(
                    &catalog, &context, &state, active, &scope, &phase,
                )?;
                assert_eq!(
                    serde_json::to_value(&prepared.model)?,
                    serde_json::to_value(expected)?,
                );
                drop(prepared);
                drop(phase);
                assert_eq!(ledger.report()?.observed_bytes, 0);
            }
        }
    }
    Ok(())
}

#[test]
fn turn_model_material_keeps_original_exact_limits_and_last_shared_holder() -> Result<()> {
    let catalog = catalog()?;
    let context = ContextConfig::default();
    let state = AgentState::new("turn-model-budget");
    let scope = InputEvidenceScope {
        agent_id: state.id.clone(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let parent_bytes = ledger.report()?.observed_bytes;
    let parent = Arc::downgrade(&phase.guard()?);
    let prepared = prepare_agent_model_state_for_turn(
        &catalog,
        &context,
        &state,
        Some(&catalog.fallback_models[0]),
        &scope,
        &phase,
    )?;
    let usage = phase.finish()?;
    let material = Arc::downgrade(&prepared._material);
    let cached = prepared.clone();
    let held = ledger.report()?.observed_bytes;
    drop(phase);
    assert!(parent.upgrade().is_none());
    drop(prepared);
    assert!(material.upgrade().is_some());
    assert_eq!(ledger.report()?.observed_bytes, held - parent_bytes);
    drop(cached);
    assert!(material.upgrade().is_none());
    assert_eq!(ledger.report()?.observed_bytes, 0);
    for (bytes, records, fits) in [
        (usage.0, usage.1, true),
        (usage.0 - 1, usage.1, false),
        (usage.0, usage.1 - 1, false),
    ] {
        let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, bytes, records)?;
        let result = prepare_agent_model_state_for_turn(
            &catalog,
            &context,
            &state,
            Some(&catalog.fallback_models[0]),
            &scope,
            &phase,
        );
        if fits {
            drop(result?);
            assert_eq!(phase.finish()?, usage);
        } else {
            assert_eq!(
                format!("{:#}", result.unwrap_err()),
                "ResourceExhausted: context resource read material",
            );
            let held = ledger.report()?.observed_bytes;
            assert!(prepare_agent_model_state_for_turn(
                &catalog, &context, &state, None, &scope, &phase,
            )
            .is_err());
            assert_eq!(ledger.report()?.observed_bytes, held);
        }
        drop(phase);
        assert_eq!(ledger.report()?.observed_bytes, 0);
    }
    Ok(())
}

#[test]
fn turn_model_material_rejects_foreign_scope_before_any_selection() -> Result<()> {
    let catalog = catalog()?;
    let context = ContextConfig::default();
    let state = AgentState::new("turn-model-scope");
    let scope = InputEvidenceScope {
        agent_id: state.id.clone(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, u64::MAX, usize::MAX)?;
    let foreign = InputEvidenceScope {
        identity_incarnation: 2,
        ..scope.clone()
    };
    let before = phase.finish()?;
    let error =
        prepare_agent_model_state_for_turn(&catalog, &context, &state, None, &foreign, &phase)
            .unwrap_err();
    assert_eq!(
        error.to_string(),
        "turn model material belongs to another scope"
    );
    assert_eq!(phase.finish()?, before);
    Ok(())
}
