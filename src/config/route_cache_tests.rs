use super::*;
use crate::model_catalog::{BuiltInModelRoutePolicy, ModelRouteCacheCapabilities};

fn cache_override(value: Option<bool>) -> ModelRouteRuntimeOverride {
    ModelRouteRuntimeOverride {
        cache_capabilities: Some(ModelRouteCacheCapabilities {
            cache_control: value,
        }),
    }
}

fn cache_catalog() -> RuntimeModelCatalog {
    let providers = built_in_provider_registry_with_settings(&HashMap::new()).unwrap();
    RuntimeModelCatalog {
        provider_endpoints: providers
            .into_iter()
            .map(|(id, runtime)| resolved_provider_endpoint_config(id, runtime).unwrap())
            .collect(),
        ..Default::default()
    }
}

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
fn route_cache_schema_get_set_unset_and_config_roundtrip() {
    let mut config = HolonConfigFile::default();
    assert!(config_schema()
        .iter()
        .any(|entry| entry.key == "models.routes"));
    assert_eq!(get_config_key(&config, "models.routes").unwrap(), json!({}));
    let routes = json!({
        "private@production/unknown/model": {
            "cache_capabilities": { "cache_control": false }
        },
        "dashscope@token-plan/qwen3.7-max": {
            "cache_capabilities": { "cache_control": true }
        },
        "private@preview/unknown": {
            "cache_capabilities": { "cache_control": null }
        },
        "private@other/unknown": { "cache_capabilities": null }
    });
    set_config_key(&mut config, "models.routes", &routes.to_string()).unwrap();
    let effective = resolve_model_route_overrides(&config).unwrap();
    assert_eq!(effective.len(), 4);
    assert_eq!(
        effective[&ModelRouteRef::parse("private@production/unknown/model").unwrap()]
            .anthropic_cache_control(),
        Some(false)
    );
    for encoded in [
        serde_json::to_string(&config).unwrap(),
        serde_json::to_string(
            &toml::from_str::<HolonConfigFile>(&toml::to_string(&config).unwrap()).unwrap(),
        )
        .unwrap(),
    ] {
        let decoded: HolonConfigFile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(resolve_model_route_overrides(&decoded).unwrap(), effective);
    }
    assert!(serde_json::to_value(&config).unwrap()["models"]["routes"].is_object());
    unset_config_key(&mut config, "models.routes").unwrap();
    assert!(config.models.is_empty());
    assert!(serde_json::to_value(&config)
        .unwrap()
        .get("models")
        .is_none());
}

#[test]
fn route_cache_rejects_invalid_fields_types_refs_and_patterns() {
    for routes in [
        json!({"private/model": {}}),
        json!({"private@production/": {}}),
        json!({"private@production/model*": {}}),
        json!({"private@production/model?": {}}),
        json!({"private@production/model name": {}}),
        json!({"private@production/model": {"unknown": true}}),
        json!({"private@production/model": {"cache_capabilities": {"unknown": true}}}),
        json!({"private@production/model": {"cache_capabilities": {"cache_control": "false"}}}),
        json!({"private@production/model": {"cache_capabilities": {"cache_control": 0}}}),
        json!({"private@production/model": null}),
        json!([]),
        Value::Null,
    ] {
        let mut config = HolonConfigFile::default();
        assert!(
            set_config_key(&mut config, "models.routes", &routes.to_string()).is_err(),
            "{routes}"
        );
        assert!(config.models.routes.is_empty());
    }
    for value in [
        json!({"cache_capabilities": {"cache_control": "false"}}),
        json!({"cache_capabilities": {"unsupported": false}}),
        json!({"unsupported": false}),
    ] {
        assert!(serde_json::from_value::<ModelRouteRuntimeOverride>(value).is_err());
    }
    for policy in [
        json!({}),
        json!({"cache_capabilities": null}),
        json!({
            "cache_capabilities": {"cache_control": null}
        }),
    ] {
        assert_eq!(
            serde_json::from_value::<ModelRouteRuntimeOverride>(policy)
                .unwrap()
                .anthropic_cache_control(),
            None
        );
    }
}

#[test]
fn route_cache_aliases_canonicalize_and_reject_conflicts() {
    let alias = "dashscope-token-plan@token-plan/qwen-3.7";
    let canonical = "dashscope@token-plan/qwen3.7-max";
    let mut config = HolonConfigFile::default();
    config
        .models
        .routes
        .insert(alias.into(), cache_override(Some(false)));
    config
        .models
        .routes
        .insert(canonical.into(), cache_override(Some(false)));
    let resolved = resolve_model_route_overrides(&config).unwrap();
    assert_eq!(resolved.len(), 1);
    assert_eq!(
        resolved[&ModelRouteRef::parse(canonical).unwrap()].anthropic_cache_control(),
        Some(false)
    );
    let mut catalog = cache_catalog();
    catalog.model_route_overrides = resolved;
    assert_eq!(
        cache_policy(&catalog, alias),
        cache_policy(&catalog, canonical)
    );
    config
        .models
        .routes
        .insert(canonical.into(), cache_override(Some(true)));
    let error = resolve_model_route_overrides(&config)
        .unwrap_err()
        .to_string();
    assert!(error.contains("conflicting models.routes aliases"));
    assert!(error.contains(canonical));
    assert!(set_config_key(
        &mut HolonConfigFile::default(),
        "models.routes",
        &serde_json::to_string(&config.models.routes).unwrap(),
    )
    .is_err());
}

#[test]
fn route_cache_precedence_covers_both_boolean_directions_and_null_inheritance() {
    let route = ModelRouteRef::parse("dashscope@token-plan/route-cache-fixture").unwrap();
    for builtin_value in [false, true] {
        let mut catalog = cache_catalog();
        let endpoint = catalog
            .provider_endpoints
            .iter_mut()
            .find(|endpoint| {
                endpoint.provider == route.provider && endpoint.endpoint == route.endpoint
            })
            .unwrap();
        let endpoint_id = endpoint.runtime_config.id.clone();
        endpoint
            .runtime_config
            .context_management
            .cache_capabilities
            .cache_control = !builtin_value;
        let builtin: BuiltInModelRoutePolicy = serde_json::from_value(json!({
            "capabilities": {},
            "cache_capabilities": {"cache_control": builtin_value}
        }))
        .unwrap();
        catalog
            .built_in_catalog
            .insert_route_policy(route.clone(), builtin);
        assert_eq!(
            cache_policy(&catalog, &route.as_string()),
            ResolvedAnthropicCachePolicy {
                cache_control: builtin_value,
                source: AnthropicCacheControlSource::RouteBuiltin,
            }
        );
        catalog
            .provider_cache_overrides
            .insert(endpoint_id.clone(), !builtin_value);
        assert_eq!(
            cache_policy(&catalog, &route.as_string()),
            ResolvedAnthropicCachePolicy {
                cache_control: !builtin_value,
                source: AnthropicCacheControlSource::ProviderOverride,
            }
        );
        catalog
            .model_route_overrides
            .insert(route.clone(), cache_override(Some(builtin_value)));
        assert_eq!(
            cache_policy(&catalog, &route.as_string()),
            ResolvedAnthropicCachePolicy {
                cache_control: builtin_value,
                source: AnthropicCacheControlSource::RouteOverride,
            }
        );
        catalog
            .model_route_overrides
            .insert(route.clone(), cache_override(None));
        assert_eq!(
            cache_policy(&catalog, &route.as_string()).source,
            AnthropicCacheControlSource::ProviderOverride
        );
        catalog.provider_cache_overrides.clear();
        assert_eq!(
            cache_policy(&catalog, &route.as_string()).source,
            AnthropicCacheControlSource::RouteBuiltin
        );
        catalog
            .built_in_catalog
            .insert_route_policy(route.clone(), BuiltInModelRoutePolicy::default());
        assert_eq!(
            cache_policy(&catalog, &route.as_string()),
            ResolvedAnthropicCachePolicy {
                cache_control: !builtin_value,
                source: AnthropicCacheControlSource::EndpointBuiltin,
            }
        );
    }
}

#[test]
fn route_cache_dashscope_glm53_default_is_scoped_and_overridable() {
    let mut catalog = cache_catalog();
    let route = "dashscope@token-plan/glm-5.3";
    assert_eq!(
        cache_policy(&catalog, route),
        ResolvedAnthropicCachePolicy {
            cache_control: false,
            source: AnthropicCacheControlSource::RouteBuiltin,
        }
    );
    for peer in [
        "dashscope@default/glm-5.3",
        "dashscope@token-plan/glm-5.1",
        "dashscope@token-plan/glm-5.2",
        "dashscope@token-plan/qwen3.7-max",
    ] {
        assert_eq!(
            cache_policy(&catalog, peer),
            ResolvedAnthropicCachePolicy {
                cache_control: true,
                source: AnthropicCacheControlSource::EndpointBuiltin,
            },
            "{peer}"
        );
    }
    catalog
        .provider_cache_overrides
        .insert(ProviderId::parse("dashscope-token-plan").unwrap(), true);
    assert_eq!(
        cache_policy(&catalog, route),
        ResolvedAnthropicCachePolicy {
            cache_control: true,
            source: AnthropicCacheControlSource::ProviderOverride,
        }
    );
    for enabled in [false, true] {
        catalog.model_route_overrides.insert(
            ModelRouteRef::parse(route).unwrap(),
            cache_override(Some(enabled)),
        );
        assert_eq!(
            cache_policy(&catalog, route),
            ResolvedAnthropicCachePolicy {
                cache_control: enabled,
                source: AnthropicCacheControlSource::RouteOverride,
            }
        );
    }
}

#[test]
fn route_cache_minimax_m3_defaults_are_exact_and_overridable() {
    for (provider, subscription_provider) in [
        ("minimax", "minimax-m-plan"),
        ("minimax-cn", "minimax-cn-m-plan"),
    ] {
        for (endpoint, legacy_provider, model) in [
            ("default", provider, "MiniMax-M3"),
            ("default", provider, "MiniMax-M3.1-Flash"),
            ("m-plan", subscription_provider, "MiniMax-M3.1-Flash"),
            (
                "m-plan",
                subscription_provider,
                "MiniMax-M3.1-Flash-Preview",
            ),
        ] {
            let mut catalog = cache_catalog();
            let route = format!("{provider}@{endpoint}/{model}");
            assert_eq!(
                cache_policy(&catalog, &route),
                ResolvedAnthropicCachePolicy {
                    cache_control: false,
                    source: AnthropicCacheControlSource::RouteBuiltin,
                },
                "{route}"
            );
            catalog
                .provider_cache_overrides
                .insert(ProviderId::parse(legacy_provider).unwrap(), true);
            assert_eq!(
                cache_policy(&catalog, &route),
                ResolvedAnthropicCachePolicy {
                    cache_control: true,
                    source: AnthropicCacheControlSource::ProviderOverride,
                }
            );
            for enabled in [false, true] {
                catalog.model_route_overrides.insert(
                    ModelRouteRef::parse(&route).unwrap(),
                    cache_override(Some(enabled)),
                );
                assert_eq!(
                    cache_policy(&catalog, &route),
                    ResolvedAnthropicCachePolicy {
                        cache_control: enabled,
                        source: AnthropicCacheControlSource::RouteOverride,
                    }
                );
            }
        }
    }
    let catalog = cache_catalog();
    for peer in [
        "minimax@default/MiniMax-M2.5",
        "minimax@default/MiniMax-M2.7",
        "minimax-cn@default/unreviewed",
        "minimax@m-plan/unreviewed",
        "dashscope@token-plan/glm-5.1",
    ] {
        assert_eq!(
            cache_policy(&catalog, peer),
            ResolvedAnthropicCachePolicy {
                cache_control: true,
                source: AnthropicCacheControlSource::EndpointBuiltin,
            },
            "{peer}"
        );
    }
}

#[test]
fn route_cache_isolates_models_endpoints_providers_and_ignores_other_transports() {
    let mut catalog = cache_catalog();
    let a = "dashscope@token-plan/cache-a";
    let b = "dashscope@token-plan/cache-b";
    catalog.model_route_overrides.insert(
        ModelRouteRef::parse(a).unwrap(),
        cache_override(Some(false)),
    );
    for (route, expected) in [(a, false), (b, true), (a, false)] {
        assert_eq!(cache_policy(&catalog, route).cache_control, expected);
    }
    assert!(cache_policy(&catalog, "dashscope@default/cache-a").cache_control);
    assert!(cache_policy(&catalog, "minimax@default/cache-a").cache_control);
    let mut custom = catalog
        .provider_endpoints
        .iter()
        .find(|endpoint| {
            endpoint.provider.as_str() == "dashscope" && endpoint.endpoint.as_str() == "token-plan"
        })
        .unwrap()
        .clone();
    custom.provider = ProviderId::parse("private").unwrap();
    custom.endpoint = ProviderEndpointId::parse("production").unwrap();
    custom.runtime_config.id = custom.provider.clone();
    custom.runtime_config.route_provider = custom.provider.clone();
    custom.runtime_config.route_endpoint = custom.endpoint.clone();
    catalog.provider_endpoints.push(custom);
    assert_eq!(
        cache_policy(&catalog, "private@production/unknown"),
        ResolvedAnthropicCachePolicy {
            cache_control: true,
            source: AnthropicCacheControlSource::CompatibilityDefault,
        }
    );
    catalog.model_route_overrides.insert(
        ModelRouteRef::parse("private@production/unknown").unwrap(),
        cache_override(Some(false)),
    );
    assert!(!cache_policy(&catalog, "private@production/unknown").cache_control);
    catalog.model_route_overrides.insert(
        ModelRouteRef::parse("openai@default/gpt-5.4").unwrap(),
        cache_override(Some(false)),
    );
    let openai = catalog
        .resolve_explicit_model_metadata(
            &ContextConfig::default(),
            &ModelRouteRef::parse("openai@default/gpt-5.4").unwrap(),
        )
        .unwrap();
    assert!(openai.capabilities.endpoint.anthropic_cache.is_none());
}

#[test]
fn route_cache_builtin_policy_and_resolved_source_serde_are_sparse() {
    let policy = serde_json::to_value(BuiltInModelRoutePolicy::default()).unwrap();
    assert!(policy.get("cache_capabilities").is_none());
    let old: crate::model_catalog::EndpointModelPolicy =
        serde_json::from_value(json!({"accepted_parameters": []})).unwrap();
    assert!(old.anthropic_cache.is_none());
    assert!(serde_json::to_value(old)
        .unwrap()
        .get("anthropic_cache")
        .is_none());
    assert_eq!(
        serde_json::to_value(ResolvedAnthropicCachePolicy {
            cache_control: false,
            source: AnthropicCacheControlSource::RouteOverride,
        })
        .unwrap(),
        json!({"cache_control": false, "source": "route_override"})
    );
    assert!(serde_json::from_value::<BuiltInModelRoutePolicy>(json!({
        "capabilities": {},
        "cache_capabilities": {"unsupported": true}
    }))
    .is_err());
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
