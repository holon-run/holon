use super::*;
use crate::context::ContextConfig;
use crate::model_catalog::{AnthropicCacheControlSource, ResolvedAnthropicCachePolicy};
use serde_json::json;

fn cache_policy(catalog: &RuntimeModelCatalog, route: &str) -> ResolvedAnthropicCachePolicy {
    catalog
        .resolve_explicit_model_metadata(
            &ContextConfig::default(),
            &ModelRouteRef::parse(route).unwrap(),
        )
        .unwrap()
        .capabilities
        .endpoint
        .anthropic_cache
        .unwrap()
}

#[test]
fn route_cache_runtime_load_preserves_provider_provenance_and_plan_inheritance() {
    let _env = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let mut stored = HolonConfigFile::default();
    stored.model.default = Some("dashscope@token-plan/qwen3.7-max".into());
    set_config_key(
        &mut stored,
        "providers.dashscope-token-plan.cache_capabilities.cache_control",
        "false",
    )
    .unwrap();
    set_config_key(
        &mut stored,
        "models.routes",
        r#"{"dashscope@token-plan/qwen3.7-max":{"cache_capabilities":{"cache_control":true}}}"#,
    )
    .unwrap();
    let path = persisted_config_path(home.path());
    save_persisted_config_at(&path, &stored).unwrap();
    let config =
        AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf())).unwrap();
    let mut catalog = RuntimeModelCatalog::from_config(&config);
    assert_eq!(
        cache_policy(&catalog, "dashscope@token-plan/qwen3.7-max"),
        ResolvedAnthropicCachePolicy {
            cache_control: true,
            source: AnthropicCacheControlSource::RouteOverride,
        }
    );
    catalog.model_route_overrides.clear();
    assert_eq!(
        cache_policy(&catalog, "dashscope@token-plan/qwen3.7-max"),
        ResolvedAnthropicCachePolicy {
            cache_control: false,
            source: AnthropicCacheControlSource::ProviderOverride,
        }
    );
    assert!(cache_policy(&catalog, "dashscope@default/qwen3.7-max").cache_control);
    let provider = stored
        .providers
        .get_mut(&ProviderId::parse("dashscope-token-plan").unwrap())
        .unwrap();
    provider.plans.insert(
        "preview".into(),
        ProviderPlanConfigFile {
            endpoint: Some(ProviderEndpointId::default_endpoint()),
        },
    );
    let overrides = explicit_provider_cache_overrides(&stored);
    assert_eq!(
        overrides[&ProviderId::parse("dashscope-token-plan-preview").unwrap()],
        false
    );
    unset_config_key(
        &mut stored,
        "providers.dashscope-token-plan.cache_capabilities.cache_control",
    )
    .unwrap();
    assert!(explicit_provider_cache_overrides(&stored).is_empty());
}

#[test]
fn route_cache_runtime_load_inherits_replaced_plan_alias_provenance() {
    let _env = crate::test_env::lock_env();
    for enabled in [false, true] {
        let home = tempfile::tempdir().unwrap();
        let stored: HolonConfigFile = serde_json::from_value(json!({
            "model": {"default": "acme-plan@default/model"},
            "providers": {
                "acme": {
                    "transport": "anthropic_messages",
                    "base_url": "https://example.invalid",
                    "auth": {"source": "none", "kind": "none"},
                    "cache_capabilities": {"cache_control": enabled},
                    "plans": {"plan": {"endpoint": "default"}}
                },
                "acme-plan": {
                    "transport": "anthropic_messages",
                    "base_url": "https://example.invalid",
                    "auth": {"source": "none", "kind": "none"},
                    "plans": {"next": {"endpoint": "default"}}
                }
            }
        }))
        .unwrap();
        save_persisted_config_at(&persisted_config_path(home.path()), &stored).unwrap();
        let config =
            AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf()))
                .unwrap();
        assert_eq!(
            config.providers[&ProviderId::parse("acme-plan-next").unwrap()]
                .context_management
                .cache_capabilities
                .cache_control,
            enabled
        );
        assert_eq!(
            cache_policy(
                &RuntimeModelCatalog::from_config(&config),
                "acme-plan@default/model"
            ),
            ResolvedAnthropicCachePolicy {
                cache_control: enabled,
                source: AnthropicCacheControlSource::ProviderOverride,
            }
        );
    }
}

#[test]
fn route_cache_runtime_load_rejects_invalid_keys_and_alias_conflicts() {
    let _env = crate::test_env::lock_env();
    let home = tempfile::tempdir().unwrap();
    let path = persisted_config_path(home.path());
    for routes in [
        json!({"dashscope/qwen3.7-max": {}}),
        json!({
            "dashscope-token-plan@token-plan/qwen-3.7": {
                "cache_capabilities": {"cache_control": false}
            },
            "dashscope@token-plan/qwen3.7-max": {
                "cache_capabilities": {"cache_control": true}
            }
        }),
    ] {
        let mut stored = HolonConfigFile::default();
        stored.models.routes = serde_json::from_value(routes).unwrap();
        save_persisted_config_at(&path, &stored).unwrap();
        let error =
            AppConfig::load_with_home_for_config_inspection(Some(home.path().to_path_buf()))
                .unwrap_err()
                .to_string();
        assert!(
            error.contains("models.routes"),
            "unexpected config load error: {error}"
        );
    }
}
