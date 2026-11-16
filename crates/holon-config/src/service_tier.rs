use super::*;

pub use holon_domain::model_values::ServiceTier;

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelRouteOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<ServiceTier>,
}

#[derive(Debug, thiserror::Error)]
#[error("model route {0} does not support service_tier")]
pub struct ServiceTierValidationError(String);

impl ServiceTierValidationError {
    #[doc(hidden)]
    pub fn new(route: String) -> Self {
        Self(route)
    }
}

impl ResolvedModelRoute {
    pub fn validate_service_tier(&self, _tier: ServiceTier) -> Result<()> {
        if self
            .capabilities
            .endpoint
            .accepted_parameters
            .iter()
            .any(|p| p.name == "service_tier")
        {
            return Ok(());
        }
        Err(ServiceTierValidationError::new(self.route_ref.as_string()).into())
    }
}

#[doc(hidden)]
pub fn service_tier_supported(route: &ModelRouteRef, endpoint: &ProviderRuntimeConfig) -> bool {
    // Sharing an OpenAI wire family does not imply sharing its service tiers.
    let Ok(url) = url::Url::parse(&endpoint.base_url) else {
        return false;
    };
    let official_endpoint = match (route.provider.as_str(), endpoint.transport) {
        ("openai-codex", ProviderTransportKind::OpenAiCodexResponses) => {
            url.scheme() == "https"
                && url.host_str() == Some("chatgpt.com")
                && url.path().trim_end_matches('/') == "/backend-api/codex"
        }
        (
            "openai",
            ProviderTransportKind::OpenAiResponses | ProviderTransportKind::OpenAiChatCompletions,
        ) => {
            url.scheme() == "https"
                && url.host_str() == Some("api.openai.com")
                && url.path().trim_end_matches('/') == "/v1"
        }
        _ => false,
    };
    official_endpoint
        && matches!(
            route.model.as_str(),
            "gpt-6.1-sol" | "gpt-6-astra" | "gpt-6-sol" | "gpt-6-luna" | "gpt-5.6-sol" | "gpt-5.5"
        )
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
    Ok(())
}
