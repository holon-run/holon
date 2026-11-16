use anyhow::{anyhow, Result};

use super::{HolonConfigFile, ProviderConfigFile, ProviderEndpointId, ProviderId};

pub use holon_provider::config::ProviderHeaders;

pub(super) fn validate_headers_for_provider(
    headers: &ProviderHeaders,
    provider: &ProviderId,
) -> Result<()> {
    let (canonical, _) = super::built_in_provider_endpoint_identity(provider)?;
    headers.validate_for_route(&canonical)
}

pub(crate) fn validate_provider_headers(
    provider: &ProviderId,
    config: &ProviderConfigFile,
) -> Result<()> {
    validate_headers_for_provider(&config.headers, provider)?;
    for endpoint in config.endpoints.values() {
        validate_headers_for_provider(&endpoint.headers, provider)?;
        config.headers.merged(&endpoint.headers)?;
    }
    Ok(())
}

#[doc(hidden)]
pub fn effective_provider_headers(
    provider: &ProviderConfigFile,
    endpoint: &ProviderEndpointId,
) -> Result<ProviderHeaders> {
    match provider.endpoints.get(endpoint) {
        Some(overrides) => provider.headers.merged(&overrides.headers),
        None => Ok(provider.headers.clone()),
    }
}

/// Upsert only the selected layer after validating the entire change.
pub fn set_provider_headers(
    config: &mut HolonConfigFile,
    provider: &ProviderId,
    endpoint: Option<&ProviderEndpointId>,
    inputs: &[String],
) -> Result<Vec<String>> {
    let updates = ProviderHeaders::new(
        inputs
            .iter()
            .map(|input| {
                input
                    .split_once('=')
                    .map(|(name, value)| (name.to_string(), value.to_string()))
                    .ok_or_else(|| anyhow!("provider headers require NAME=VALUE"))
            })
            .collect::<Result<Vec<_>>>()?,
    )?;
    validate_headers_for_provider(&updates, provider)?;
    let mut candidate = provider_config_for_header_update(config, provider, endpoint)?;
    let layer = match endpoint {
        Some(endpoint) => {
            &mut candidate
                .endpoints
                .entry(endpoint.clone())
                .or_default()
                .headers
        }
        None => &mut candidate.headers,
    };
    *layer = layer.merged(&updates)?;
    super::validate_provider_config(provider, &candidate)?;
    config.providers.insert(provider.clone(), candidate);
    Ok(updates.names())
}

/// Delete explicit entries only; endpoint deletion restores inheritance.
pub fn unset_provider_headers(
    config: &mut HolonConfigFile,
    provider: &ProviderId,
    endpoint: Option<&ProviderEndpointId>,
    names: &[String],
) -> Result<Vec<String>> {
    let names = ProviderHeaders::new(names.iter().map(|name| (name.clone(), String::new())))?;
    validate_headers_for_provider(&names, provider)?;
    let names = names.names();
    let mut candidate = provider_config_for_header_update(config, provider, endpoint)?;
    let layer = match endpoint {
        Some(endpoint) => {
            &mut candidate
                .endpoints
                .entry(endpoint.clone())
                .or_default()
                .headers
        }
        None => &mut candidate.headers,
    };
    *layer = layer.without(&names);
    super::validate_provider_config(provider, &candidate)?;
    config.providers.insert(provider.clone(), candidate);
    Ok(names)
}

fn provider_config_for_header_update(
    config: &HolonConfigFile,
    provider: &ProviderId,
    endpoint: Option<&ProviderEndpointId>,
) -> Result<ProviderConfigFile> {
    let candidate = if let Some(existing) = config.providers.get(provider) {
        existing.clone()
    } else {
        let mut defaults = super::built_in_provider_default_config(provider)?
            .ok_or_else(|| anyhow!("provider {} not found", provider.as_str()))?;
        // Unlike `providers set`, a header-only edit must preserve built-in auth.
        let registry = super::built_in_provider_registry()?;
        defaults.auth = registry
            .get(provider)
            .ok_or_else(|| anyhow!("provider {} not found", provider.as_str()))?
            .auth
            .clone();
        defaults
    };
    if let Some(endpoint) = endpoint {
        let (canonical, _) = super::built_in_provider_endpoint_identity(provider)?;
        let registered = holon_domain::model_values::provider_definitions()
            .iter()
            .any(|definition| {
                definition.route_provider == canonical.as_str()
                    && definition.route_endpoint == endpoint.as_str()
            });
        if endpoint != &ProviderEndpointId::default_endpoint()
            && !candidate.endpoints.contains_key(endpoint)
            && !registered
        {
            return Err(anyhow!(
                "provider {} has no endpoint {}",
                provider.as_str(),
                endpoint.as_str()
            ));
        }
    }
    Ok(candidate)
}

#[cfg(test)]
mod tests;
