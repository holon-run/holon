use super::*;
use crate::{
    diagnostics::resource_io::{charge_owned_metadata, ResourceReadMaterial},
    runtime::{
        input_evidence_budget::InputEvidenceBudgetLedger,
        resident_input_evidence::InputEvidenceScope,
    },
};
use std::sync::Arc;

#[test]
fn turn_chain_selection_borrows_original_routes_without_material() -> Result<()> {
    let primary = ModelRouteRef::parse("openai@default/gpt-5.4")?;
    let fallback = ModelRouteRef::parse("deepseek@responses/deepseek-v4-pro")?;
    let unknown = ModelRouteRef::parse("openai@other/outside-chain")?;
    let mut catalog = RuntimeModelCatalog {
        default_model: primary.clone(),
        fallback_models: vec![fallback.clone(), primary, fallback],
        ..Default::default()
    };
    let scope = InputEvidenceScope {
        agent_id: "borrowed-turn-chain".into(),
        identity_incarnation: 1,
    };
    let ledger = InputEvidenceBudgetLedger::new(Default::default())?;
    let baseline = ResourceReadMaterial::new(&ledger, &scope)?;
    let header = baseline.finish()?;
    drop(baseline);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, header.0, header.1)?;
    phase.read(|| {
        let mut routes = ModelListCatalog::from(&catalog)
            .provider_chain_for_turn_refs(None, Some(&catalog.fallback_models[0]));
        assert!(std::ptr::eq(
            routes.next().unwrap(),
            &catalog.fallback_models[0]
        ));
        assert!(routes.next().is_none());
        let mut outside =
            ModelListCatalog::from(&catalog).provider_chain_for_turn_refs(None, Some(&unknown));
        assert!(std::ptr::eq(outside.next().unwrap(), &unknown));
        assert!(outside.next().is_none());
        Ok(())
    })?;
    catalog.disable_provider_fallback = true;
    phase.read(|| {
        let mut routes = ModelListCatalog::from(&catalog).provider_chain_for_turn_refs(None, None);
        assert!(std::ptr::eq(routes.next().unwrap(), &catalog.default_model));
        assert!(routes.next().is_none());
        Ok(())
    })?;
    assert_eq!(phase.finish()?, header);
    Ok(())
}

fn config(home: &std::path::Path) -> Result<AppConfig> {
    std::fs::write(
        home.join("config.json"),
        r#"{"model":{"default":"openai/gpt-5.4"}}"#,
    )?;
    AppConfig::load_with_home(Some(home.to_path_buf()))
}

#[test]
fn list_catalog_borrows_original_config_and_existing_catalog_fields() -> Result<()> {
    let home = tempfile::tempdir()?;
    let config = config(home.path())?;
    let view = ModelListCatalog::from_config(&config);
    assert!(std::ptr::eq(view.default_model, &config.default_model));
    assert!(std::ptr::eq(
        view.fallback_models,
        config.fallback_models.as_slice()
    ));
    assert!(std::ptr::eq(
        view.route_options,
        &config.stored_config.model.route_options
    ));
    assert!(std::ptr::eq(
        view.built_in_catalog,
        BuiltInModelCatalog::shared()
    ));
    let mut catalog = RuntimeModelCatalog::from_config(&config);
    catalog.default_model = ModelRouteRef::parse("other@remote/model-汉字")?;
    catalog.disable_provider_fallback = !config.disable_provider_fallback;
    let view = ModelListCatalog::from(&catalog);
    assert!(std::ptr::eq(view.default_model, &catalog.default_model));
    assert!(std::ptr::eq(
        view.fallback_models,
        catalog.fallback_models.as_slice()
    ));
    assert!(std::ptr::eq(view.route_options, &catalog.route_options));
    assert!(std::ptr::eq(
        view.built_in_catalog,
        &catalog.built_in_catalog
    ));
    assert_eq!(
        view.disable_provider_fallback,
        catalog.disable_provider_fallback
    );
    Ok(())
}

#[test]
fn list_catalog_chain_borrows_first_wins_routes_and_respects_disabled_fallback() -> Result<()> {
    let home = tempfile::tempdir()?;
    let mut config = config(home.path())?;
    let fallback = ModelRouteRef::parse("openai@remote/fallback-汉字")?;
    let same_model_other_endpoint = ModelRouteRef::parse("openai@another/fallback-汉字")?;
    config.fallback_models = vec![
        config.default_model.clone(),
        fallback.clone(),
        fallback.clone(),
        same_model_other_endpoint.clone(),
    ];
    let override_model = ModelRouteRef::parse("mistral@remote/override")?;
    for disabled in [false, true] {
        config.disable_provider_fallback = disabled;
        for override_route in [None, Some(&override_model)] {
            let view = ModelListCatalog::from_config(&config);
            let expected = if disabled {
                vec![override_route.unwrap_or(&config.default_model)]
            } else if override_route.is_some() {
                vec![
                    &override_model,
                    &config.fallback_models[0],
                    &config.fallback_models[1],
                    &config.fallback_models[3],
                ]
            } else {
                vec![
                    &config.default_model,
                    &config.fallback_models[1],
                    &config.fallback_models[3],
                ]
            };
            let actual: Vec<_> = view.provider_chain_refs(override_route).collect();
            assert_eq!(actual, expected);
            for (actual, expected) in actual.into_iter().zip(expected) {
                assert!(std::ptr::eq(actual, expected));
            }
        }
    }
    Ok(())
}

#[test]
fn list_catalog_borrowing_neither_charges_nor_resets_original_phase() -> Result<()> {
    let home = tempfile::tempdir()?;
    let config = config(home.path())?;
    let catalog = RuntimeModelCatalog::from_config(&config);
    let ledger = InputEvidenceBudgetLedger::new(StateMemoryLimits::default())?;
    let scope = InputEvidenceScope {
        agent_id: "borrowed-list-catalog".into(),
        identity_incarnation: 1,
    };
    let baseline = ResourceReadMaterial::new(&ledger, &scope)?;
    let initial = baseline.finish()?;
    drop(baseline);
    let phase = ResourceReadMaterial::with_test_limits(&ledger, &scope, initial.0, initial.1)?;
    let holder = phase.guard()?;
    {
        let _entry = phase.enter();
        for _ in 0..3 {
            assert!(std::ptr::eq(
                ModelListCatalog::from_config(&config).default_model,
                &config.default_model
            ));
            assert!(std::ptr::eq(
                ModelListCatalog::from(&catalog).default_model,
                &catalog.default_model
            ));
        }
        assert_eq!(phase.finish()?, initial);
        assert!(Arc::ptr_eq(
            &ResourceReadMaterial::active_guard()?.unwrap(),
            &holder
        ));
        assert!(charge_owned_metadata(1, 1).is_err());
        assert_eq!(
            ModelListCatalog::from_config(&config).default_model,
            &config.default_model
        );
        assert_eq!(
            ModelListCatalog::from(&catalog).default_model,
            &catalog.default_model
        );
        assert!(phase.finish().is_err());
        assert!(ResourceReadMaterial::active_guard().is_err());
    }
    drop(phase);
    assert_eq!(ledger.report()?.observed_bytes, initial.0);
    drop(holder);
    assert_eq!(ledger.report()?.observed_bytes, 0);
    Ok(())
}
