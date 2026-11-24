use anyhow::Result;

use super::{
    HolonConfigFile, ProviderConfigFile, ProviderDiscoveryConfig, ProviderEndpointId, ProviderId,
    ProviderRuntimeConfig,
};

/// Effective discovery configuration for one endpoint.
///
/// Resolution order is fixed: the endpoint entry wins over the provider-level
/// default. `None` means "nothing declared", so the transport/built-in default
/// still applies; an explicit `disabled` kind is returned as configured and
/// turns discovery off.
pub fn provider_discovery_for(
    provider: &ProviderConfigFile,
    endpoint: &ProviderEndpointId,
) -> Option<ProviderDiscoveryConfig> {
    provider
        .endpoints
        .get(endpoint)
        .and_then(|endpoint| endpoint.discovery.clone())
        .or_else(|| provider.discovery.clone())
}

/// Effective discovery configuration for a resolved endpoint runtime.
pub fn resolved_provider_discovery(
    stored: &HolonConfigFile,
    provider: &ProviderRuntimeConfig,
) -> Option<ProviderDiscoveryConfig> {
    // The runtime registry key is the configured provider id (a built-in alias
    // or a plan alias like `gateway-anthropic`). Look there first, then at the
    // canonical provider block that owns plan endpoints.
    let configured = stored
        .providers
        .get(&provider.id)
        .or_else(|| stored.providers.get(&provider.route_provider))?;
    provider_discovery_for(configured, &provider.route_endpoint)
}

pub(crate) fn validate_provider_discovery(
    provider_id: &ProviderId,
    config: &ProviderConfigFile,
) -> Result<()> {
    if let Some(discovery) = config.discovery.as_ref() {
        discovery.validate(&format!("providers.{}.discovery", provider_id.as_str()))?;
    }
    for (endpoint_id, endpoint) in &config.endpoints {
        if let Some(discovery) = endpoint.discovery.as_ref() {
            discovery.validate(&format!(
                "providers.{}.endpoints.{}.discovery",
                provider_id.as_str(),
                endpoint_id.as_str()
            ))?;
        }
    }
    Ok(())
}
