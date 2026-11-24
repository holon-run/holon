use super::*;

pub use holon_domain::model_values::ServiceTier;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelRouteOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceTierValidationError {
    #[error("model route {0} does not support service_tier")]
    UnsupportedRoute(String),
    #[error(
        "model route {route} does not support service_tier '{requested}'; supported values: \
         {supported}"
    )]
    UnsupportedValue {
        route: String,
        requested: String,
        supported: String,
    },
}

impl ServiceTierValidationError {
    #[doc(hidden)]
    pub fn new(route: String) -> Self {
        Self::UnsupportedRoute(route)
    }
}

impl ResolvedModelRoute {
    pub fn validate_service_tier(&self, tier: ServiceTier) -> Result<()> {
        let declared = self
            .capabilities
            .endpoint
            .accepted_parameters
            .iter()
            .find(|p| p.name == "service_tier")
            .map(|parameter| parameter.allowed_values.as_slice());
        validate_declared_service_tier(&self.route_ref.as_string(), declared, tier)
    }
}

/// Validate a tier against the contract the route advertises.
///
/// Validating against the same `allowed_values` clients read keeps one source
/// of truth: a tier cannot be accepted while the route advertises fewer tiers.
#[doc(hidden)]
pub fn validate_declared_service_tier(
    route: &str,
    declared_allowed_values: Option<&[String]>,
    tier: ServiceTier,
) -> Result<()> {
    let Some(allowed_values) = declared_allowed_values else {
        return Err(ServiceTierValidationError::UnsupportedRoute(route.to_string()).into());
    };
    let requested = tier.preference_value();
    if allowed_values.iter().any(|value| value == requested) {
        return Ok(());
    }
    Err(ServiceTierValidationError::UnsupportedValue {
        route: route.to_string(),
        requested: requested.to_string(),
        supported: allowed_values.join(", "),
    }
    .into())
}

/// Service tiers a single route advertises, independent of each other.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SupportedServiceTiers {
    pub fast: bool,
    pub ultrafast: bool,
}

impl SupportedServiceTiers {
    pub fn is_empty(&self) -> bool {
        !self.fast && !self.ultrafast
    }

    /// Preference values for `accepted_parameters.allowed_values`.
    ///
    /// `default` is always available where `service_tier` is declared at all.
    pub fn allowed_values(&self) -> Vec<String> {
        let mut values = vec![ServiceTier::Default.preference_value().to_string()];
        if self.fast {
            values.push(ServiceTier::Fast.preference_value().to_string());
        }
        if self.ultrafast {
            values.push(ServiceTier::Ultrafast.preference_value().to_string());
        }
        values
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServiceTierEndpoint {
    OpenAiApi,
    OpenAiCodex,
}

/// Fast and Ultrafast model lists stay separate on purpose: one is not a
/// superset of the other, and #4460 owns collapsing them into catalog metadata.
const FAST_MODELS: &[&str] = &[
    "gpt-6.2-sol",
    "gpt-6.2-astra",
    "gpt-6.1-sol",
    "gpt-6.1-terra",
    "gpt-6-astra",
    "gpt-6-sol",
    "gpt-6-luna",
    "gpt-5.6-sol",
    "gpt-5.5",
];

const ULTRAFAST_MODELS: &[&str] = &["gpt-6.2-sol", "gpt-6.2-astra", "gpt-6-astra", "gpt-6.1-sol"];

fn service_tier_endpoint(
    route: &ModelRouteRef,
    endpoint: &ProviderRuntimeConfig,
) -> Option<ServiceTierEndpoint> {
    // Sharing an OpenAI wire family does not imply sharing its service tiers.
    let url = url::Url::parse(&endpoint.base_url).ok()?;
    match (route.provider.as_str(), endpoint.transport) {
        ("openai-codex", ProviderTransportKind::OpenAiCodexResponses)
            if url.scheme() == "https"
                && url.host_str() == Some("chatgpt.com")
                && url.path().trim_end_matches('/') == "/backend-api/codex" =>
        {
            Some(ServiceTierEndpoint::OpenAiCodex)
        }
        (
            "openai",
            ProviderTransportKind::OpenAiResponses | ProviderTransportKind::OpenAiChatCompletions,
        ) if url.scheme() == "https"
            && url.host_str() == Some("api.openai.com")
            && url.path().trim_end_matches('/') == "/v1" =>
        {
            Some(ServiceTierEndpoint::OpenAiApi)
        }
        _ => None,
    }
}

#[doc(hidden)]
pub fn supported_service_tiers(
    route: &ModelRouteRef,
    endpoint: &ProviderRuntimeConfig,
) -> SupportedServiceTiers {
    let Some(endpoint_kind) = service_tier_endpoint(route, endpoint) else {
        return SupportedServiceTiers::default();
    };
    let model = route.model.as_str();
    let ultrafast = match endpoint_kind {
        // Codex Ultrafast is not verified against the live backend, so it stays
        // undeclared instead of advertising a tier the route cannot confirm.
        ServiceTierEndpoint::OpenAiCodex => false,
        ServiceTierEndpoint::OpenAiApi => ULTRAFAST_MODELS.contains(&model),
    };
    SupportedServiceTiers {
        fast: FAST_MODELS.contains(&model),
        ultrafast,
    }
}

pub fn validate_model_route_options(catalog: &RuntimeModelCatalog) -> Result<()> {
    for (key, options) in &catalog.route_options {
        let route_ref = ModelRouteRef::parse(key)?;
        if catalog.canonicalize_model_route_ref(&route_ref).as_string() != *key {
            return Err(anyhow!(
                "model.route_options key must be a canonical model route: {key}"
            ));
        }
        let route = catalog
            .resolve_explicit_model_route(
                &ContextConfig::default(),
                &route_ref,
                ModelRouteCapability::Turn,
            )
            .ok_or_else(|| anyhow!("model.route_options route is not configured: {key}"))?;
        if let Some(tier) = options.service_tier {
            route.validate_service_tier(tier)?;
        }
    }
    // `models.routes` keys must reference an existing provider endpoint. An
    // unknown endpoint is a configuration error, not something to silently
    // ignore, otherwise the cache override applies to a route that can never
    // resolve.
    for route_ref in catalog.model_route_overrides.keys() {
        catalog
            .try_resolve_explicit_model_metadata(&ContextConfig::default(), route_ref)
            .map_err(|error| {
                anyhow!(
                    "invalid models.routes key {}: {error}",
                    route_ref.as_string()
                )
            })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_config(transport: ProviderTransportKind, base_url: &str) -> ProviderRuntimeConfig {
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: ProviderId::openai_codex(),
            route_provider: ProviderId::openai_codex(),
            route_endpoint: crate::ProviderEndpointId::default_endpoint(),
            transport,
            base_url: base_url.into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::None,
                kind: CredentialKind::None,
                env: None,
                profile: None,
                external: None,
                client_id: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        }
    }

    fn route(value: &str) -> ModelRouteRef {
        ModelRouteRef::parse_compatible(value).unwrap()
    }

    fn official_codex() -> ProviderRuntimeConfig {
        runtime_config(
            ProviderTransportKind::OpenAiCodexResponses,
            "https://chatgpt.com/backend-api/codex",
        )
    }

    #[test]
    fn codex_fast_mode_is_limited_to_supported_models_on_the_official_endpoint() {
        let official = official_codex();
        // Fast availability is per model, not per provider: newer generations
        // keep arriving after the original list.
        for model in [
            "gpt-6.2-sol",
            "gpt-6.2-astra",
            "gpt-6.1-sol",
            "gpt-6.1-terra",
            "gpt-6-astra",
            "gpt-6-sol",
            "gpt-6-luna",
            "gpt-5.6-sol",
        ] {
            let route = route(&format!("openai-codex@default/{model}"));
            assert_eq!(
                supported_service_tiers(&route, &official),
                SupportedServiceTiers {
                    fast: true,
                    ultrafast: false
                },
                "{model} should declare Fast but not Ultrafast"
            );
        }
        assert!(
            supported_service_tiers(&route("openai-codex@default/gpt-9-unknown"), &official)
                .is_empty()
        );

        let proxy = runtime_config(
            ProviderTransportKind::OpenAiCodexResponses,
            "https://gateway.example/v1",
        );
        assert!(
            supported_service_tiers(&route("openai-codex@default/gpt-6.2-sol"), &proxy).is_empty()
        );
    }

    #[test]
    fn openai_api_advertises_fast_and_ultrafast_per_model_on_the_official_endpoint() {
        for transport in [
            ProviderTransportKind::OpenAiResponses,
            ProviderTransportKind::OpenAiChatCompletions,
        ] {
            let official = runtime_config(transport, "https://api.openai.com/v1");
            assert_eq!(
                supported_service_tiers(&route("openai@default/gpt-6.2-sol"), &official),
                SupportedServiceTiers {
                    fast: true,
                    ultrafast: true
                }
            );
            // Fast-only: Ultrafast must not leak onto routes that do not list it.
            assert_eq!(
                supported_service_tiers(&route("openai@default/gpt-5.5"), &official),
                SupportedServiceTiers {
                    fast: true,
                    ultrafast: false
                }
            );
            assert!(
                supported_service_tiers(&route("openai@default/gpt-9-unknown"), &official)
                    .is_empty()
            );
        }
        let proxy = runtime_config(
            ProviderTransportKind::OpenAiResponses,
            "https://api.openai.com.evil.example/v1",
        );
        assert!(supported_service_tiers(&route("openai@default/gpt-6.2-sol"), &proxy).is_empty());
    }

    #[test]
    fn allowed_values_follow_the_advertised_tiers() {
        assert_eq!(
            SupportedServiceTiers {
                fast: true,
                ultrafast: true
            }
            .allowed_values(),
            vec!["default", "fast", "ultrafast"]
        );
        assert_eq!(
            SupportedServiceTiers {
                fast: true,
                ultrafast: false
            }
            .allowed_values(),
            vec!["default", "fast"]
        );
        assert!(SupportedServiceTiers::default().allowed_values() == vec!["default"]);
    }

    fn advertised(
        route_value: &str,
        transport: ProviderTransportKind,
        base_url: &str,
    ) -> Vec<String> {
        supported_service_tiers(&route(route_value), &runtime_config(transport, base_url))
            .allowed_values()
    }

    #[test]
    fn validation_rejects_values_the_route_does_not_advertise() {
        let openai = advertised(
            "openai@default/gpt-6.2-sol",
            ProviderTransportKind::OpenAiResponses,
            "https://api.openai.com/v1",
        );
        for tier in [
            ServiceTier::Default,
            ServiceTier::Fast,
            ServiceTier::Ultrafast,
        ] {
            assert!(
                validate_declared_service_tier("openai@default/gpt-6.2-sol", Some(&openai), tier)
                    .is_ok(),
                "{tier:?} should be accepted on the Ultrafast route"
            );
        }

        let fast_only = advertised(
            "openai@default/gpt-5.5",
            ProviderTransportKind::OpenAiResponses,
            "https://api.openai.com/v1",
        );
        assert!(validate_declared_service_tier(
            "openai@default/gpt-5.5",
            Some(&fast_only),
            ServiceTier::Fast
        )
        .is_ok());
        let error = validate_declared_service_tier(
            "openai@default/gpt-5.5",
            Some(&fast_only),
            ServiceTier::Ultrafast,
        )
        .expect_err("Fast-only route must reject ultrafast");
        let message = error.to_string();
        assert!(message.contains("ultrafast"), "{message}");
        assert!(message.contains("gpt-5.5"), "{message}");
        assert!(message.contains("default, fast"), "{message}");

        let codex = advertised(
            "openai-codex@default/gpt-6.2-sol",
            ProviderTransportKind::OpenAiCodexResponses,
            "https://chatgpt.com/backend-api/codex",
        );
        assert!(validate_declared_service_tier(
            "openai-codex@default/gpt-6.2-sol",
            Some(&codex),
            ServiceTier::Fast
        )
        .is_ok());
        assert!(validate_declared_service_tier(
            "openai-codex@default/gpt-6.2-sol",
            Some(&codex),
            ServiceTier::Ultrafast
        )
        .is_err());
    }

    #[test]
    fn validation_rejects_routes_without_the_parameter() {
        let proxy = advertised(
            "openai@default/gpt-6.2-sol",
            ProviderTransportKind::OpenAiResponses,
            "https://gateway.example/v1",
        );
        assert_eq!(proxy, vec!["default"]);
        assert!(validate_declared_service_tier(
            "openai@default/gpt-6.2-sol",
            None,
            ServiceTier::Default
        )
        .is_err());
    }
}
