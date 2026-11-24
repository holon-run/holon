use super::*;
pub(crate) use holon_domain::model_values::built_in_provider_endpoint_identity;
use holon_domain::model_values::{
    provider_definitions, ProviderContextManagement, ProviderDefinition, ProviderMaterializer,
    ProviderWebSearch,
};

pub fn validate_provider_config(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
    validate_provider_headers(provider_id, provider_config)?;
    validate_provider_discovery(provider_id, provider_config)?;
    let effective = provider_config_for_default_endpoint(provider_config);
    parse_url_value("providers.<id>.base_url", &effective.base_url)?;
    validate_provider_auth(provider_id, &effective.auth)?;
    for (endpoint_id, endpoint_config) in &provider_config.endpoints {
        if let Some(base_url) = endpoint_config.base_url.as_deref() {
            parse_url_value(
                &format!(
                    "providers.{}.endpoints.{}.base_url",
                    provider_id.as_str(),
                    endpoint_id.as_str()
                ),
                base_url,
            )?;
        }
        if let Some(auth) = &endpoint_config.auth {
            validate_provider_auth(provider_id, auth)?;
        }
    }
    validate_provider_builtin_web_search(provider_id, &effective)
}

pub(crate) fn persisted_provider_config_mut<'a>(
    config: &'a mut HolonConfigFile,
    key: &str,
    suffix: &str,
) -> Result<&'a mut ProviderConfigFile> {
    let rest = key
        .strip_prefix("providers.")
        .and_then(|value| value.strip_suffix(suffix))
        .ok_or_else(|| unknown_config_key(key))?;
    if rest.is_empty() {
        return Err(anyhow!(
            "providers.<id>{suffix} requires a non-empty provider id"
        ));
    }
    let id = ProviderId::parse(rest)?;
    persisted_provider_config_by_id_mut(config, id)
}

pub(crate) fn persisted_provider_config_by_id_mut(
    config: &mut HolonConfigFile,
    id: ProviderId,
) -> Result<&mut ProviderConfigFile> {
    if !config.providers.contains_key(&id) {
        let defaults = built_in_provider_default_config(&id)?
            .ok_or_else(|| anyhow!("provider {} not found", id.as_str()))?;
        config.providers.insert(id.clone(), defaults);
    }
    config
        .providers
        .get_mut(&id)
        .ok_or_else(|| anyhow!("provider {} not found", id.as_str()))
}

fn provider_endpoint_key<'a>(rest: &'a str, suffix: &str) -> Option<(&'a str, &'a str)> {
    let rest = rest.strip_suffix(suffix)?;
    rest.split_once(".endpoints.")
}

/// Match `providers.<id>.endpoints.<name>` (the whole entry, no leaf field).
fn provider_endpoint_entry_key(rest: &str) -> Option<(&str, &str)> {
    let (id, endpoint) = rest.split_once(".endpoints.")?;
    if id.is_empty() || endpoint.is_empty() || endpoint.contains('.') {
        return None;
    }
    Some((id, endpoint))
}

fn provider_plan_key<'a>(rest: &'a str, suffix: &str) -> Option<(&'a str, &'a str)> {
    let rest = rest.strip_suffix(suffix)?;
    rest.split_once(".plans.")
}

fn provider_endpoint_transport(
    provider: &ProviderConfigFile,
    endpoint: &ProviderEndpointId,
) -> Option<ProviderTransportKind> {
    provider
        .endpoints
        .get(endpoint)
        .and_then(|endpoint| endpoint.transport)
        .or_else(|| endpoint_is_default(endpoint).then_some(provider.transport))
}

fn provider_endpoint_base_url<'a>(
    provider: &'a ProviderConfigFile,
    endpoint: &ProviderEndpointId,
) -> Option<&'a str> {
    provider
        .endpoints
        .get(endpoint)
        .and_then(|endpoint| endpoint.base_url.as_deref())
        .or_else(|| endpoint_is_default(endpoint).then_some(provider.base_url.as_str()))
}

fn provider_endpoint_auth<'a>(
    provider: &'a ProviderConfigFile,
    endpoint: &ProviderEndpointId,
) -> Option<&'a ProviderAuthConfig> {
    provider
        .endpoints
        .get(endpoint)
        .and_then(|endpoint| endpoint.auth.as_ref())
        .or_else(|| endpoint_is_default(endpoint).then_some(&provider.auth))
}

fn endpoint_is_default(endpoint: &ProviderEndpointId) -> bool {
    endpoint.as_str() == ProviderEndpointId::DEFAULT
}

fn provider_config_for_default_endpoint(
    provider_config: &ProviderConfigFile,
) -> ProviderConfigFile {
    let mut effective = provider_config.clone();
    if let Some(default_endpoint) = provider_config
        .endpoints
        .get(&ProviderEndpointId::default_endpoint())
    {
        if let Some(transport) = default_endpoint.transport {
            effective.transport = transport;
        }
        if let Some(base_url) = &default_endpoint.base_url {
            effective.base_url = base_url.clone();
        }
        if let Some(auth) = &default_endpoint.auth {
            effective.auth = auth.clone();
        }
    }
    effective
}

pub(crate) fn get_provider_config_key(config: &HolonConfigFile, key: &str) -> Result<Value> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    if rest.is_empty() {
        return Err(anyhow!("providers.<id> requires a non-empty provider id"));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".transport") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_transport(provider, &endpoint))
            .map(|transport| Value::String(transport.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".discovery.kind") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.endpoints.get(&endpoint))
            .and_then(|endpoint| endpoint.discovery.as_ref())
            .map(|discovery| Value::String(discovery.kind.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".discovery.path") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.endpoints.get(&endpoint))
            .and_then(|endpoint| endpoint.discovery.as_ref())
            .and_then(|discovery| discovery.path.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".base_url") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_base_url(provider, &endpoint))
            .map(|value| Value::String(value.to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".auth.source") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_auth(provider, &endpoint))
            .map(|auth| Value::String(auth.source.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".auth.kind") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_auth(provider, &endpoint))
            .map(|auth| Value::String(auth.kind.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".auth.env") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_auth(provider, &endpoint))
            .and_then(|auth| auth.env.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".auth.profile") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_auth(provider, &endpoint))
            .and_then(|auth| auth.profile.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".auth.external") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_auth(provider, &endpoint))
            .and_then(|auth| auth.external.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, endpoint)) = provider_endpoint_key(rest, ".auth.client_id") {
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider_endpoint_auth(provider, &endpoint))
            .and_then(|auth| auth.client_id.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some((id, plan)) = provider_plan_key(rest, ".endpoint") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.plans.get(plan))
            .and_then(|plan| plan.endpoint.as_ref())
            .map(|endpoint| Value::String(endpoint.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".transport") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.transport.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".base_url") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.base_url.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.source") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.auth.source.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.kind") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .map(|provider| Value::String(provider.auth.kind.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.env") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.env.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.profile") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.profile.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.external") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.external.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".auth.client_id") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.auth.client_id.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".cache_capabilities.cache_control") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.cache_capabilities.as_ref())
            .map(|capabilities| Value::Bool(capabilities.cache_control))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".discovery.kind") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.discovery.as_ref())
            .map(|discovery| Value::String(discovery.kind.as_str().to_string()))
            .unwrap_or(Value::Null));
    }
    if let Some(id) = rest.strip_suffix(".discovery.path") {
        return Ok(config
            .providers
            .get(&ProviderId::parse(id)?)
            .and_then(|provider| provider.discovery.as_ref())
            .and_then(|discovery| discovery.path.as_ref())
            .map(|value| Value::String(value.clone()))
            .unwrap_or(Value::Null));
    }
    if rest.contains('.') {
        return Err(unknown_config_key(key));
    }
    Ok(config
        .providers
        .get(&ProviderId::parse(rest)?)
        .map(serde_json::to_value)
        .transpose()?
        .unwrap_or(Value::Null))
}

pub(crate) fn set_provider_config_key(
    config: &mut HolonConfigFile,
    key: &str,
    raw_value: &str,
) -> Result<()> {
    if set_provider_endpoint_config_key(config, key, raw_value)? {
        return Ok(());
    }
    if set_provider_plan_config_key(config, key, raw_value)? {
        return Ok(());
    }
    if key.ends_with(".discovery.kind") {
        let kind = ProviderDiscoveryKind::parse(raw_value)?;
        let provider = persisted_provider_config_mut(config, key, ".discovery.kind")?;
        let discovery = provider
            .discovery
            .get_or_insert_with(|| ProviderDiscoveryConfig { kind, path: None });
        discovery.kind = kind;
        // `disabled` never carries a path; switching kind must not leave the
        // rejected `disabled` + `path` pair behind in the stored config.
        if kind == ProviderDiscoveryKind::Disabled {
            discovery.path = None;
        }
        return Ok(());
    }
    if key.ends_with(".discovery.path") {
        let value = raw_value.trim().to_string();
        let provider = persisted_provider_config_mut(config, key, ".discovery.path")?;
        if value.is_empty() {
            // An empty path cannot describe `openai_models`, so this drops the
            // block exactly like `unset` instead of persisting an invalid pair.
            provider.discovery = None;
            return Ok(());
        }
        let discovery = provider
            .discovery
            .get_or_insert_with(|| ProviderDiscoveryConfig::openai_models("/models"));
        if discovery.kind == ProviderDiscoveryKind::Disabled {
            return Err(anyhow!(
                "{key} cannot be set while discovery kind is disabled; unset discovery or set the kind first"
            ));
        }
        let discovery = ProviderDiscoveryConfig {
            kind: discovery.kind,
            path: Some(value),
        };
        discovery.validate(key.strip_suffix(".path").unwrap_or(key))?;
        provider.discovery = Some(discovery);
        return Ok(());
    }
    if key.ends_with(".transport") {
        persisted_provider_config_mut(config, key, ".transport")?.transport =
            ProviderTransportKind::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".base_url") {
        let value = raw_value.trim();
        parse_url_value(key, value)?;
        persisted_provider_config_mut(config, key, ".base_url")?.base_url = value.to_string();
        return Ok(());
    }
    if key.ends_with(".auth.source") {
        persisted_provider_config_mut(config, key, ".auth.source")?
            .auth
            .source = CredentialSource::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".auth.kind") {
        persisted_provider_config_mut(config, key, ".auth.kind")?
            .auth
            .kind = CredentialKind::parse(raw_value)?;
        return Ok(());
    }
    if key.ends_with(".auth.env") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.env")?
            .auth
            .env = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.profile") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.profile")?
            .auth
            .profile = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.external") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.external")?
            .auth
            .external = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".auth.client_id") {
        let value = raw_value.trim();
        persisted_provider_config_mut(config, key, ".auth.client_id")?
            .auth
            .client_id = (!value.is_empty()).then(|| value.to_string());
        return Ok(());
    }
    if key.ends_with(".cache_capabilities.cache_control") {
        let value =
            parse_bool_value(raw_value)?.ok_or_else(|| anyhow!("{key} expects a boolean"))?;
        persisted_provider_config_mut(config, key, ".cache_capabilities.cache_control")?
            .cache_capabilities = Some(AnthropicCacheCapabilities {
            cache_control: value,
        });
        return Ok(());
    }
    Err(unknown_config_key(key))
}

pub(crate) fn unset_provider_config_key(config: &mut HolonConfigFile, key: &str) -> Result<()> {
    if unset_provider_endpoint_config_key(config, key)? {
        return Ok(());
    }
    if unset_provider_plan_config_key(config, key)? {
        return Ok(());
    }
    if key.ends_with(".discovery.kind") {
        persisted_provider_config_mut(config, key, ".discovery.kind")?.discovery = None;
        return Ok(());
    }
    if key.ends_with(".discovery.path") {
        persisted_provider_config_mut(config, key, ".discovery.path")?.discovery = None;
        return Ok(());
    }
    if key.ends_with(".auth.env") {
        persisted_provider_config_mut(config, key, ".auth.env")?
            .auth
            .env = None;
        return Ok(());
    }
    if key.ends_with(".auth.profile") {
        persisted_provider_config_mut(config, key, ".auth.profile")?
            .auth
            .profile = None;
        return Ok(());
    }
    if key.ends_with(".auth.external") {
        persisted_provider_config_mut(config, key, ".auth.external")?
            .auth
            .external = None;
        return Ok(());
    }
    if key.ends_with(".auth.client_id") {
        persisted_provider_config_mut(config, key, ".auth.client_id")?
            .auth
            .client_id = None;
        return Ok(());
    }
    if key.ends_with(".cache_capabilities.cache_control") {
        persisted_provider_config_mut(config, key, ".cache_capabilities.cache_control")?
            .cache_capabilities = None;
        return Ok(());
    }
    let id = key
        .strip_prefix("providers.")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("providers.<id> requires a non-empty provider id"))?;
    if config.providers.remove(&ProviderId::parse(id)?).is_none() {
        return Err(anyhow!("provider {id} not found"));
    }
    Ok(())
}

fn set_provider_endpoint_config_key(
    config: &mut HolonConfigFile,
    key: &str,
    raw_value: &str,
) -> Result<bool> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".discovery.kind") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let kind = ProviderDiscoveryKind::parse(raw_value)?;
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint = provider.endpoints.entry(endpoint_id).or_default();
        let discovery = endpoint
            .discovery
            .get_or_insert_with(|| ProviderDiscoveryConfig { kind, path: None });
        discovery.kind = kind;
        // `disabled` never carries a path; switching kind must not leave the
        // rejected `disabled` + `path` pair behind in the stored config.
        if kind == ProviderDiscoveryKind::Disabled {
            discovery.path = None;
        }
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".discovery.path") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let value = raw_value.trim();
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint = provider.endpoints.entry(endpoint_id).or_default();
        if value.is_empty() {
            // An empty path cannot describe `openai_models`, so this drops the
            // block exactly like `unset` instead of persisting an invalid pair.
            endpoint.discovery = None;
            return Ok(true);
        }
        let discovery = endpoint
            .discovery
            .get_or_insert_with(|| ProviderDiscoveryConfig::openai_models("/models"));
        if discovery.kind == ProviderDiscoveryKind::Disabled {
            return Err(anyhow!(
                "{key} cannot be set while discovery kind is disabled; unset discovery or set the kind first"
            ));
        }
        let discovery = ProviderDiscoveryConfig {
            kind: discovery.kind,
            path: Some(value.to_string()),
        };
        discovery.validate(key.strip_suffix(".path").unwrap_or(key))?;
        endpoint.discovery = Some(discovery);
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".transport") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider.endpoints.entry(endpoint_id).or_default().transport =
            Some(ProviderTransportKind::parse(raw_value)?);
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".base_url") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let value = raw_value.trim();
        parse_url_value(key, value)?;
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider.endpoints.entry(endpoint_id).or_default().base_url = Some(value.to_string());
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.source") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider
            .endpoints
            .entry(endpoint_id)
            .or_default()
            .auth
            .get_or_insert_with(ProviderAuthConfig::default)
            .source = CredentialSource::parse(raw_value)?;
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.kind") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider
            .endpoints
            .entry(endpoint_id)
            .or_default()
            .auth
            .get_or_insert_with(ProviderAuthConfig::default)
            .kind = CredentialKind::parse(raw_value)?;
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.env") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let value = raw_value.trim();
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider
            .endpoints
            .entry(endpoint_id)
            .or_default()
            .auth
            .get_or_insert_with(ProviderAuthConfig::default)
            .env = (!value.is_empty()).then(|| value.to_string());
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.profile") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let value = raw_value.trim();
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider
            .endpoints
            .entry(endpoint_id)
            .or_default()
            .auth
            .get_or_insert_with(ProviderAuthConfig::default)
            .profile = (!value.is_empty()).then(|| value.to_string());
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.external") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let value = raw_value.trim();
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider
            .endpoints
            .entry(endpoint_id)
            .or_default()
            .auth
            .get_or_insert_with(ProviderAuthConfig::default)
            .external = (!value.is_empty()).then(|| value.to_string());
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.client_id") {
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        let value = raw_value.trim();
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let inherited_auth = provider_config_for_default_endpoint(provider).auth;
        provider
            .endpoints
            .entry(endpoint_id)
            .or_default()
            .auth
            .get_or_insert(inherited_auth)
            .client_id = (!value.is_empty()).then(|| value.to_string());
        return Ok(true);
    }
    Ok(false)
}

fn set_provider_plan_config_key(
    config: &mut HolonConfigFile,
    key: &str,
    raw_value: &str,
) -> Result<bool> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    let Some((id, plan_id)) = provider_plan_key(rest, ".endpoint") else {
        return Ok(false);
    };
    if plan_id.is_empty() {
        return Err(anyhow!(
            "providers.<id>.plans.<plan_id> requires a non-empty plan id"
        ));
    }
    let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
    provider
        .plans
        .entry(plan_id.to_string())
        .or_default()
        .endpoint = Some(ProviderEndpointId::parse(raw_value)?);
    Ok(true)
}

fn unset_provider_endpoint_config_key(config: &mut HolonConfigFile, key: &str) -> Result<bool> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".discovery.kind")
        .or_else(|| provider_endpoint_key(rest, ".discovery.path"))
    {
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        if let Some(endpoint) = provider.endpoints.get_mut(&endpoint_id) {
            endpoint.discovery = None;
        }
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".transport") {
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        if let Some(endpoint) = provider.endpoints.get_mut(&endpoint_id) {
            endpoint.transport = None;
        }
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".base_url") {
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        if let Some(endpoint) = provider.endpoints.get_mut(&endpoint_id) {
            endpoint.base_url = None;
        }
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.source") {
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        if let Some(auth) = provider
            .endpoints
            .get_mut(&endpoint_id)
            .and_then(|endpoint| endpoint.auth.as_mut())
        {
            auth.source = CredentialSource::None;
        }
        return Ok(true);
    }
    if let Some((id, endpoint_id)) = provider_endpoint_key(rest, ".auth.kind") {
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
        if let Some(auth) = provider
            .endpoints
            .get_mut(&endpoint_id)
            .and_then(|endpoint| endpoint.auth.as_mut())
        {
            auth.kind = CredentialKind::None;
        }
        return Ok(true);
    }
    for suffix in [
        ".auth.env",
        ".auth.profile",
        ".auth.external",
        ".auth.client_id",
    ] {
        if let Some((id, endpoint_id)) = provider_endpoint_key(rest, suffix) {
            let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
            let endpoint_id = ProviderEndpointId::parse(endpoint_id)?;
            if let Some(auth) = provider
                .endpoints
                .get_mut(&endpoint_id)
                .and_then(|endpoint| endpoint.auth.as_mut())
            {
                match suffix {
                    ".auth.env" => auth.env = None,
                    ".auth.profile" => auth.profile = None,
                    ".auth.external" => auth.external = None,
                    ".auth.client_id" => auth.client_id = None,
                    _ => {}
                }
            }
            return Ok(true);
        }
    }
    // Whole-entry removal: `config unset providers.<id>.endpoints.<name>` drops
    // the endpoint subtree (transport/base_url/auth/headers) instead of only
    // clearing one leaf, so a stale stub cannot linger with no supported path
    // back to a clean state.
    if let Some((id, endpoint_id)) = provider_endpoint_entry_key(rest) {
        let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
        provider
            .endpoints
            .remove(&ProviderEndpointId::parse(endpoint_id)?);
        return Ok(true);
    }
    Ok(false)
}

fn unset_provider_plan_config_key(config: &mut HolonConfigFile, key: &str) -> Result<bool> {
    let rest = key
        .strip_prefix("providers.")
        .ok_or_else(|| unknown_config_key(key))?;
    let Some((id, plan_id)) = provider_plan_key(rest, ".endpoint") else {
        return Ok(false);
    };
    let provider = persisted_provider_config_by_id_mut(config, ProviderId::parse(id)?)?;
    if let Some(plan) = provider.plans.get_mut(plan_id) {
        plan.endpoint = None;
    }
    Ok(true)
}

pub fn built_in_provider_default_config(
    provider_id: &ProviderId,
) -> Result<Option<ProviderConfigFile>> {
    let settings_env = load_settings_env()?;
    built_in_provider_default_config_with_settings(provider_id, &settings_env)
}

#[doc(hidden)]
pub fn built_in_provider_default_config_with_settings(
    provider_id: &ProviderId,
    settings_env: &HashMap<String, String>,
) -> Result<Option<ProviderConfigFile>> {
    let catalog = BuiltInProviderCatalog::with_settings(settings_env)?;
    Ok(catalog
        .legacy_runtime(provider_id)
        .map(|provider| ProviderConfigFile {
            transport: provider.transport,
            base_url: provider.base_url.clone(),
            // Copilot onboarding needs its OAuth profile before a credential exists.
            auth: if provider_id.as_str() == "github-copilot" {
                provider.auth.clone()
            } else {
                ProviderAuthConfig::default()
            },
            headers: ProviderHeaders::default(),
            reasoning_effort: None,
            builtin_web_search: None,
            cache_capabilities: None,
            discovery: None,
            endpoints: BTreeMap::new(),
            plans: BTreeMap::new(),
        }))
}

/// Provider metadata for documentation generation.
#[derive(Debug, Clone)]
pub struct ProviderDocEntry {
    pub provider: ProviderId,
    pub endpoint: ProviderEndpointId,
    pub legacy_provider: ProviderId,
    pub id: ProviderId,
    pub transport: ProviderTransportKind,
    pub base_url: String,
    pub auth_env: Option<String>,
}

/// Returns built-in provider metadata for documentation generation.
pub fn built_in_provider_doc_entries() -> Result<Vec<ProviderDocEntry>> {
    let settings_env = HashMap::new();
    Ok(BuiltInProviderCatalog::with_settings(&settings_env)?.doc_entries())
}

#[doc(hidden)]
pub fn resolve_provider_registry(
    stored_config: &HolonConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<ProviderRegistry> {
    let mut registry = built_in_provider_registry_with_settings(settings_env)?;
    // Template headers before any user configuration is merged; plan
    // endpoint aliases recompute their header layering from these defaults.
    let builtin_headers: HashMap<ProviderId, ProviderHeaders> = registry
        .values()
        .map(|provider| (provider.id.clone(), provider.headers.clone()))
        .collect();
    for provider in registry.values_mut() {
        provider.credential = None;
    }
    for (id, provider_config) in &stored_config.providers {
        let built_in = registry.remove(id);
        let runtime = materialize_provider_config(
            id.clone(),
            provider_config.clone(),
            settings_env,
            credential_store,
            built_in,
        )?;
        registry.insert(id.clone(), runtime);
    }
    for (id, provider_config) in &stored_config.providers {
        let Some(base_runtime) = registry.get(id).cloned() else {
            continue;
        };
        for (plan_id, plan_config) in &provider_config.plans {
            let Some(endpoint_id) = plan_config.endpoint.as_ref() else {
                continue;
            };
            if endpoint_id.as_str() != ProviderEndpointId::DEFAULT
                && !provider_config.endpoints.contains_key(endpoint_id)
            {
                return Err(anyhow!(
                    "providers.{}.plans.{}.endpoint references unknown endpoint {}",
                    id.as_str(),
                    plan_id,
                    endpoint_id.as_str()
                ));
            }
            let alias = provider_plan_alias_id(id, plan_id)?;
            let mut runtime = materialize_provider_endpoint_config(
                id,
                endpoint_id,
                provider_config,
                base_runtime.clone(),
                builtin_headers.get(id).cloned().unwrap_or_default(),
                settings_env,
                credential_store,
            )?;
            runtime.id = alias.clone();
            registry.insert(alias, runtime);
        }
    }
    for provider in registry.values_mut() {
        if let Some(config) = stored_config
            .providers
            .get(&provider.id)
            .or_else(|| stored_config.providers.get(&provider.route_provider))
        {
            let configured_headers = effective_provider_headers(config, &provider.route_endpoint)?;
            // Built-in defaults stay when the user configures no headers;
            // explicit user headers win per name.
            provider.headers = provider.headers.merged(&configured_headers)?;
            provider_headers::validate_headers_for_provider(
                &provider.headers,
                &provider.route_provider,
            )?;
        }
        if provider.credential.is_none() {
            provider.credential =
                resolve_provider_credential(&provider.auth, settings_env, credential_store)?;
        }
    }
    Ok(registry)
}

fn provider_plan_alias_id(provider_id: &ProviderId, plan_id: &str) -> Result<ProviderId> {
    if plan_id.trim().is_empty() {
        return Err(anyhow!(
            "providers.{}.plans.<plan_id> requires a non-empty plan id",
            provider_id.as_str()
        ));
    }
    ProviderId::parse(&format!("{}-{}", provider_id.as_str(), plan_id))
}

fn materialize_provider_endpoint_config(
    provider_id: &ProviderId,
    endpoint_id: &ProviderEndpointId,
    provider_config: &ProviderConfigFile,
    mut runtime: ProviderRuntimeConfig,
    template_headers: ProviderHeaders,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<ProviderRuntimeConfig> {
    // Recompute the endpoint headers from the built-in template plus the
    // provider-level and selected-endpoint layers. The incoming runtime
    // already carries the default endpoint's header layer, which must not
    // leak to an alias that routes to a different base URL.
    runtime.headers =
        template_headers.merged(&effective_provider_headers(provider_config, endpoint_id)?)?;
    if let Some(endpoint_config) = provider_config.endpoints.get(endpoint_id) {
        if let Some(transport) = endpoint_config.transport {
            runtime.transport = transport;
        }
        if let Some(base_url) = endpoint_config.base_url.as_deref() {
            parse_url_value(
                &format!(
                    "providers.{}.endpoints.{}.base_url",
                    provider_id.as_str(),
                    endpoint_id.as_str()
                ),
                base_url,
            )?;
            runtime.base_url = base_url.to_string();
        }
        if let Some(auth) = endpoint_config.auth.as_ref() {
            validate_provider_auth(provider_id, auth)?;
            runtime.auth = auth.clone();
            runtime.credential = resolve_provider_credential(auth, settings_env, credential_store)?;
        }
        if endpoint_id.as_str() != ProviderEndpointId::DEFAULT {
            runtime.builtin_web_search = None;
        }
    }
    runtime.route_provider = provider_id.clone();
    runtime.route_endpoint = endpoint_id.clone();
    Ok(runtime)
}

#[doc(hidden)]
pub fn built_in_provider_registry_with_settings(
    settings_env: &HashMap<String, String>,
) -> Result<ProviderRegistry> {
    Ok(BuiltInProviderCatalog::with_settings(settings_env)?.into_legacy_registry())
}

#[doc(hidden)]
pub fn resolved_provider_endpoint_config(
    legacy_provider: ProviderId,
    runtime_config: ProviderRuntimeConfig,
) -> Result<ResolvedProviderEndpointConfig> {
    let (provider, endpoint) = if runtime_config.route_provider.as_str().is_empty() {
        built_in_provider_endpoint_identity(&legacy_provider)?
    } else {
        (
            runtime_config.route_provider.clone(),
            runtime_config.route_endpoint.clone(),
        )
    };
    Ok(ResolvedProviderEndpointConfig {
        provider,
        endpoint,
        runtime_config,
    })
}

#[derive(Debug, Clone)]
pub(crate) struct BuiltInProviderEndpointDefinition {
    pub provider: ProviderId,
    pub endpoint: ProviderEndpointId,
    pub legacy_provider: ProviderId,
    pub runtime_config: ProviderRuntimeConfig,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct BuiltInProviderCatalog {
    endpoints: Vec<BuiltInProviderEndpointDefinition>,
}

impl BuiltInProviderCatalog {
    pub(crate) fn with_settings(settings_env: &HashMap<String, String>) -> Result<Self> {
        let mut catalog = Self::default();
        populate_built_in_provider_catalog(&mut catalog, settings_env)?;
        Ok(catalog)
    }

    pub(crate) fn insert_endpoint(
        &mut self,
        provider: ProviderId,
        endpoint: ProviderEndpointId,
        legacy_provider: ProviderId,
        mut runtime_config: ProviderRuntimeConfig,
    ) {
        runtime_config.route_provider = provider.clone();
        runtime_config.route_endpoint = endpoint.clone();
        self.endpoints.push(BuiltInProviderEndpointDefinition {
            provider,
            endpoint,
            legacy_provider,
            runtime_config,
        });
    }

    pub(crate) fn legacy_runtime(
        &self,
        provider_id: &ProviderId,
    ) -> Option<&ProviderRuntimeConfig> {
        self.endpoints
            .iter()
            .find(|endpoint| &endpoint.legacy_provider == provider_id)
            .map(|endpoint| &endpoint.runtime_config)
    }

    pub(crate) fn into_legacy_registry(self) -> ProviderRegistry {
        self.endpoints
            .into_iter()
            .map(|endpoint| (endpoint.legacy_provider, endpoint.runtime_config))
            .collect()
    }

    pub(crate) fn doc_entries(&self) -> Vec<ProviderDocEntry> {
        let mut entries = self
            .endpoints
            .iter()
            .map(|endpoint| ProviderDocEntry {
                provider: endpoint.provider.clone(),
                endpoint: endpoint.endpoint.clone(),
                legacy_provider: endpoint.legacy_provider.clone(),
                id: endpoint.legacy_provider.clone(),
                transport: endpoint.runtime_config.transport,
                base_url: endpoint.runtime_config.base_url.clone(),
                auth_env: endpoint.runtime_config.auth.env.clone(),
            })
            .collect::<Vec<_>>();
        entries.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        entries
    }
}

pub(crate) fn populate_built_in_provider_catalog(
    catalog: &mut BuiltInProviderCatalog,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    for definition in provider_definitions() {
        match definition.materializer {
            ProviderMaterializer::OpenAiCodex => insert_openai_codex_provider(catalog, definition)?,
            ProviderMaterializer::OpenAi => insert_openai_provider(catalog, definition)?,
            ProviderMaterializer::Anthropic => {
                insert_anthropic_provider(catalog, definition, settings_env)?
            }
            ProviderMaterializer::Gemini => {
                insert_gemini_provider(catalog, definition, settings_env)?
            }
            ProviderMaterializer::Generic => {
                insert_generic_provider(catalog, definition, settings_env)?
            }
            ProviderMaterializer::VercelAiGateway => {
                insert_vercel_ai_gateway_provider(catalog, definition, settings_env)?
            }
            ProviderMaterializer::GitHubCopilot => {
                insert_github_copilot_provider(catalog, definition, settings_env)?
            }
            ProviderMaterializer::AliasOnly => {}
        }
    }
    Ok(())
}

fn insert_definition_runtime(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
    runtime_config: ProviderRuntimeConfig,
) -> Result<()> {
    catalog.insert_endpoint(
        ProviderId::parse(definition.route_provider)?,
        ProviderEndpointId::parse(definition.route_endpoint)?,
        ProviderId::parse(definition.legacy_provider)?,
        runtime_config,
    );
    Ok(())
}

fn insert_openai_codex_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
) -> Result<()> {
    let id = ProviderId::parse(definition.legacy_provider)?;
    let reasoning_effort = env::var("HOLON_OPENAI_CODEX_REASONING_EFFORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("low".to_string()))
        .map(|value| validate_openai_reasoning_effort(&value).map(|_| value))
        .transpose()?;
    insert_definition_runtime(
        catalog,
        definition,
        ProviderRuntimeConfig {
            headers: Default::default(),
            id,
            route_provider: ProviderId::parse(definition.route_provider)?,
            route_endpoint: ProviderEndpointId::parse(definition.route_endpoint)?,
            transport: definition.transport,
            base_url: env::var("HOLON_OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| definition.default_base_url.to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
                external: Some("codex_cli".into()),
                client_id: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: Some(
                env::var("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| default_codex_home()),
            ),
            originator: Some("codex_cli_rs".into()),
            reasoning_effort,
            context_management: Default::default(),
            builtin_web_search: Some(openai_codex_builtin_web_search_config()),
        },
    )
}

fn insert_github_copilot_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let id = ProviderId::parse(definition.legacy_provider)?;
    // Real integration identity only: no editor/IDE mimicry headers.
    let headers = holon_provider::config::ProviderHeaders::new([(
        "Copilot-Integration-Id".to_string(),
        "holon".to_string(),
    )])?;
    insert_definition_runtime(
        catalog,
        definition,
        ProviderRuntimeConfig {
            headers,
            id,
            route_provider: ProviderId::parse(definition.route_provider)?,
            route_endpoint: ProviderEndpointId::parse(definition.route_endpoint)?,
            transport: definition.transport,
            base_url: get_config_value("HOLON_GITHUB_COPILOT_BASE_URL", None, settings_env)
                .unwrap_or_else(|| definition.default_base_url.to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(GITHUB_COPILOT_CREDENTIAL_PROFILE.into()),
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
        },
    )
}

fn insert_openai_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
) -> Result<()> {
    let id = ProviderId::parse(definition.legacy_provider)?;
    insert_definition_runtime(
        catalog,
        definition,
        ProviderRuntimeConfig {
            headers: Default::default(),
            id,
            route_provider: ProviderId::parse(definition.route_provider)?,
            route_endpoint: ProviderEndpointId::parse(definition.route_endpoint)?,
            transport: definition.transport,
            base_url: env::var("HOLON_OPENAI_BASE_URL")
                .ok()
                .or_else(|| env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| definition.default_base_url.to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: env::var("OPENAI_API_KEY").ok(),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(openai_builtin_web_search_config()),
        },
    )
}

fn insert_anthropic_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let id = ProviderId::parse(definition.legacy_provider)?;
    insert_definition_runtime(
        catalog,
        definition,
        ProviderRuntimeConfig {
            headers: Default::default(),
            id,
            route_provider: ProviderId::parse(definition.route_provider)?,
            route_endpoint: ProviderEndpointId::parse(definition.route_endpoint)?,
            transport: definition.transport,
            base_url: get_config_value("ANTHROPIC_BASE_URL", None, settings_env)
                .unwrap_or_else(|| definition.default_base_url.to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: get_config_value("ANTHROPIC_AUTH_TOKEN", None, settings_env),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: resolve_anthropic_context_management_config()?,
            builtin_web_search: Some(anthropic_builtin_web_search_config()),
        },
    )
}

fn insert_gemini_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let id = ProviderId::parse(definition.legacy_provider)?;
    insert_definition_runtime(
        catalog,
        definition,
        ProviderRuntimeConfig {
            headers: Default::default(),
            id,
            route_provider: ProviderId::parse(definition.route_provider)?,
            route_endpoint: ProviderEndpointId::parse(definition.route_endpoint)?,
            transport: definition.transport,
            base_url: get_config_value("HOLON_GEMINI_BASE_URL", None, settings_env)
                .unwrap_or_else(|| definition.default_base_url.to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("GEMINI_API_KEY".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: get_config_value("GEMINI_API_KEY", None, settings_env),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(gemini_builtin_web_search_config()),
        },
    )
}

fn insert_generic_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let context_management = match definition.context_management {
        ProviderContextManagement::Default => Default::default(),
        ProviderContextManagement::Anthropic => resolve_anthropic_context_management_config()?,
        ProviderContextManagement::AnthropicCompatible => {
            resolve_anthropic_compatible_context_management_config()?
        }
        ProviderContextManagement::AnthropicCompatibleImplicitCache => {
            resolve_anthropic_compatible_context_management_config()?.with_implicit_prompt_caching()
        }
    };
    let builtin_web_search = match definition.web_search {
        ProviderWebSearch::None => None,
        ProviderWebSearch::OpenAi => Some(openai_builtin_web_search_config()),
        ProviderWebSearch::OpenAiCodex => Some(openai_codex_builtin_web_search_config()),
        ProviderWebSearch::Anthropic => Some(anthropic_builtin_web_search_config()),
        ProviderWebSearch::Zai => Some(zai_builtin_web_search_config()),
        ProviderWebSearch::BigModel => Some(bigmodel_builtin_web_search_config()),
        ProviderWebSearch::DeepSeek => Some(deepseek_builtin_web_search_config()),
        ProviderWebSearch::Gemini => Some(gemini_builtin_web_search_config()),
    };
    insert_builtin_http_provider_with_context_management(
        catalog,
        definition.legacy_provider,
        definition.transport,
        definition.default_base_url,
        definition.credential_envs,
        settings_env,
        context_management,
        builtin_web_search,
        definition.default_reasoning_effort,
    )
}

/// Public entry point for tools that need the built-in provider registry (e.g. docgen).
pub fn built_in_provider_registry() -> Result<ProviderRegistry> {
    let settings_env = load_settings_env()?;
    built_in_provider_registry_with_settings(&settings_env)
}

pub(crate) fn insert_vercel_ai_gateway_provider(
    catalog: &mut BuiltInProviderCatalog,
    definition: &ProviderDefinition,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let context_management = resolve_anthropic_compatible_context_management_config()?;
    let oidc_credential = resolve_first_env_value(&["VERCEL_OIDC_TOKEN"], settings_env);
    let api_key_credential = resolve_first_env_value(
        &["AI_GATEWAY_API_KEY", "VERCEL_AI_GATEWAY_API_KEY"],
        settings_env,
    );
    let (kind, env_name, credential) = if let Some(credential) = oidc_credential {
        (
            CredentialKind::BearerToken,
            credential
                .env_name
                .clone()
                .unwrap_or_else(|| "VERCEL_OIDC_TOKEN".to_string()),
            Some(credential.value),
        )
    } else if let Some(credential) = api_key_credential {
        (
            CredentialKind::ApiKey,
            credential
                .env_name
                .clone()
                .unwrap_or_else(|| "AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY".to_string()),
            Some(credential.value),
        )
    } else {
        (
            CredentialKind::BearerToken,
            "VERCEL_OIDC_TOKEN or AI_GATEWAY_API_KEY or VERCEL_AI_GATEWAY_API_KEY".to_string(),
            None,
        )
    };
    let id = ProviderId::parse(definition.legacy_provider)?;
    insert_definition_runtime(
        catalog,
        definition,
        ProviderRuntimeConfig {
            headers: Default::default(),
            id,
            route_provider: ProviderId::parse(definition.route_provider)?,
            route_endpoint: ProviderEndpointId::parse(definition.route_endpoint)?,
            transport: definition.transport,
            base_url: get_config_value("HOLON_VERCEL_AI_GATEWAY_BASE_URL", None, settings_env)
                .unwrap_or_else(|| definition.default_base_url.to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind,
                env: Some(env_name),
                profile: None,
                external: None,
                client_id: None,
            },
            credential,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management,
            builtin_web_search: None,
        },
    )
}

fn insert_builtin_http_provider_with_context_management(
    catalog: &mut BuiltInProviderCatalog,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
    context_management: AnthropicContextManagementConfig,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
    default_reasoning_effort: Option<&str>,
) -> Result<()> {
    let id = ProviderId::parse(provider)?;
    let (provider_id, endpoint_id) = built_in_provider_endpoint_identity(&id)?;
    let base_url_env = format!("HOLON_{}_BASE_URL", env_key_fragment(provider));
    let base_url = get_config_value(&base_url_env, None, settings_env)
        .unwrap_or_else(|| default_base_url.to_string());
    let credential = resolve_first_env_value(env_names, settings_env);
    let env_name = credential
        .as_ref()
        .and_then(|resolution| resolution.env_name.clone())
        .or_else(|| {
            if env_names.is_empty() {
                None
            } else {
                Some(env_names.join(" or "))
            }
        });
    catalog.insert_endpoint(
        provider_id.clone(),
        endpoint_id.clone(),
        id.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: id.clone(),
            route_provider: provider_id,
            route_endpoint: endpoint_id,
            transport,
            base_url,
            auth: env_name
                .as_ref()
                .map(|env| ProviderAuthConfig {
                    source: CredentialSource::Env,
                    kind: CredentialKind::ApiKey,
                    env: Some(env.clone()),
                    profile: None,
                    external: None,
                    client_id: None,
                })
                .unwrap_or_default(),
            credential: credential.map(|resolution| resolution.value),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: default_reasoning_effort.map(str::to_string),
            context_management,
            builtin_web_search,
        },
    );
    Ok(())
}

pub(crate) fn openai_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::OpenAi,
        advertised_tool_type: "web_search_preview".to_string(),
        backend_kind: "openai_web_search".to_string(),
    }
}

pub(crate) fn openai_codex_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::OpenAi,
        advertised_tool_type: "web_search".to_string(),
        backend_kind: "openai_codex_web_search".to_string(),
    }
}

pub(crate) fn anthropic_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "anthropic_web_search".to_string(),
    }
}

pub(crate) fn zai_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "zai_web_search_prime".to_string(),
    }
}

pub(crate) fn bigmodel_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "bigmodel_web_search".to_string(),
    }
}

pub(crate) fn deepseek_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::DeepSeek,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "deepseek_web_search".to_string(),
    }
}

pub(crate) fn gemini_builtin_web_search_config() -> ProviderBuiltinWebSearchConfig {
    ProviderBuiltinWebSearchConfig {
        enabled: true,
        kind: ProviderNativeWebSearchKind::Gemini,
        advertised_tool_type: "google_search".to_string(),
        backend_kind: "gemini_web_search".to_string(),
    }
}

pub(crate) fn env_key_fragment(provider: &str) -> String {
    provider
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

pub(crate) fn resolve_first_env_value(
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Option<ResolvedEnvValue> {
    env_names.iter().find_map(|env_name| {
        get_config_value(env_name, None, settings_env).map(|value| ResolvedEnvValue {
            env_name: Some((*env_name).to_string()),
            value,
        })
    })
}

pub(crate) struct ResolvedEnvValue {
    env_name: Option<String>,
    value: String,
}

#[doc(hidden)]
pub fn materialize_provider_config(
    id: ProviderId,
    provider_config: ProviderConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
    built_in: Option<ProviderRuntimeConfig>,
) -> Result<ProviderRuntimeConfig> {
    validate_provider_headers(&id, &provider_config)?;
    // Discovery declares a load-time contract too: an `openai_models` entry
    // without a valid relative path must fail loudly instead of silently
    // degrading to "no discovery" at request time.
    validate_provider_discovery(&id, &provider_config)?;
    let effective_provider_config = provider_config_for_default_endpoint(&provider_config);
    validate_provider_auth(&id, &effective_provider_config.auth)?;
    let credential = resolve_provider_credential(
        &effective_provider_config.auth,
        settings_env,
        credential_store,
    )?;
    let mut runtime = built_in.unwrap_or_else(|| ProviderRuntimeConfig {
        headers: Default::default(),
        id: id.clone(),
        route_provider: id.clone(),
        route_endpoint: ProviderEndpointId::default_endpoint(),
        transport: effective_provider_config.transport,
        base_url: effective_provider_config.base_url.clone(),
        auth: effective_provider_config.auth.clone(),
        credential: None,
        credential_store_path: None,
        codex_home: None,
        originator: None,
        reasoning_effort: None,
        context_management: Default::default(),
        builtin_web_search: None,
    });
    if let Some(reasoning_effort) = provider_config.reasoning_effort.as_deref() {
        if !(id.as_str() == "tencent-tokenhub"
            && effective_provider_config.transport == ProviderTransportKind::OpenAiChatCompletions
            && reasoning_effort == "no_think")
        {
            validate_openai_reasoning_effort(reasoning_effort)?;
        }
    }
    validate_provider_builtin_web_search(&id, &effective_provider_config)?;
    runtime.id = id;
    runtime.transport = effective_provider_config.transport;
    runtime.base_url = effective_provider_config.base_url;
    runtime.auth = effective_provider_config.auth;
    let configured_headers =
        effective_provider_headers(&provider_config, &ProviderEndpointId::default_endpoint())?;
    // Built-in default headers (e.g. Copilot-Integration-Id) survive absent
    // user headers; explicit user headers win per name.
    runtime.headers = runtime.headers.merged(&configured_headers)?;
    runtime.credential = credential;
    if provider_config.reasoning_effort.is_some() {
        runtime.reasoning_effort = provider_config.reasoning_effort;
    }
    if let Some(cache_capabilities) = provider_config.cache_capabilities {
        runtime.context_management.cache_capabilities = cache_capabilities;
    }
    if let Some(builtin_web_search) = provider_config.builtin_web_search {
        runtime.builtin_web_search = builtin_web_search.enabled.then_some(builtin_web_search);
    }
    Ok(runtime)
}

pub(crate) fn validate_openai_reasoning_effort(value: &str) -> Result<()> {
    match value {
        "none" | "low" | "medium" | "high" | "xhigh" | "max" => Ok(()),
        "ultra" => Err(anyhow!(
            "OpenAI Codex reasoning_effort 'ultra' is unavailable; \
             Holon does not yet implement the required orchestration semantics"
        )),
        _ => Err(anyhow!(
            "invalid OpenAI Codex reasoning_effort '{value}'; \
             must be one of none, low, medium, high, xhigh, max"
        )),
    }
}

#[cfg(test)]
mod oauth_client_id_tests;

#[cfg(test)]
mod reasoning_effort_tests;

pub(crate) fn validate_provider_builtin_web_search(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
    let Some(search) = provider_config.builtin_web_search.as_ref() else {
        return Ok(());
    };
    if !search.enabled {
        return Ok(());
    }
    if search.advertised_tool_type.trim().is_empty() {
        return Err(anyhow!(
            "providers.{}.builtin_web_search.advertised_tool_type must not be empty",
            provider_id.as_str()
        ));
    }
    if search.backend_kind.trim().is_empty() {
        return Err(anyhow!(
            "providers.{}.builtin_web_search.backend_kind must not be empty",
            provider_id.as_str()
        ));
    }
    match (provider_config.transport, search.kind) {
        (ProviderTransportKind::OpenAiResponses, ProviderNativeWebSearchKind::OpenAi) => {
            if search.advertised_tool_type == "web_search_preview" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search_preview for OpenAI Responses native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::OpenAiResponses, ProviderNativeWebSearchKind::Xai) => {
            Err(anyhow!(
                "providers.{}.builtin_web_search cannot enable xAI hosted search; use the isolated XSearch tool",
                provider_id.as_str()
            ))
        }
        (ProviderTransportKind::OpenAiCodexResponses, ProviderNativeWebSearchKind::OpenAi) => {
            if search.advertised_tool_type == "web_search" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search for OpenAI Codex Responses native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::AnthropicMessages, ProviderNativeWebSearchKind::Anthropic) => {
            if search.advertised_tool_type == "web_search_20250305" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search_20250305 for Anthropic Messages native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::AnthropicMessages, ProviderNativeWebSearchKind::DeepSeek) => {
            if search.advertised_tool_type == "web_search_20250305" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search_20250305 for DeepSeek Anthropic-compatible native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::OpenAiResponses, ProviderNativeWebSearchKind::DeepSeek) => {
            if matches!(
                search.advertised_tool_type.as_str(),
                "web_search" | "web_search_2025_08_26"
            ) {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be web_search or web_search_2025_08_26 for DeepSeek Responses native search",
                    provider_id.as_str()
                ))
            }
        }
        (ProviderTransportKind::GeminiGenerateContent, ProviderNativeWebSearchKind::Gemini) => {
            if search.advertised_tool_type == "google_search" {
                Ok(())
            } else {
                Err(anyhow!(
                    "providers.{}.builtin_web_search.advertised_tool_type must be google_search for Gemini native search",
                    provider_id.as_str()
                ))
            }
        }
        _ => Err(anyhow!(
            "providers.{}.builtin_web_search kind {:?} is incompatible with transport {:?}",
            provider_id.as_str(),
            search.kind,
            provider_config.transport
        )),
    }
}

pub(crate) fn resolve_provider_credential(
    auth: &ProviderAuthConfig,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<Option<String>> {
    match auth.source {
        CredentialSource::Env => Ok(auth
            .env
            .as_deref()
            .and_then(|key| get_config_value(key, None, settings_env))),
        CredentialSource::AuthProfile => auth
            .profile
            .as_deref()
            .map(normalize_credential_profile_id)
            .transpose()?
            .and_then(|profile| credential_store.profiles.get(&profile))
            .map(|entry| {
                if entry.kind != auth.kind {
                    return Err(anyhow!(
                        "credential profile {} has kind {}, but provider expects {}",
                        auth.profile.as_deref().unwrap_or_default(),
                        entry.kind.as_str(),
                        auth.kind.as_str()
                    ));
                }
                Ok(entry.material.clone())
            })
            .transpose(),
        CredentialSource::None
        | CredentialSource::ExternalCli
        | CredentialSource::CredentialProcess => Ok(None),
    }
}

#[doc(hidden)]
pub fn validate_provider_auth(provider_id: &ProviderId, auth: &ProviderAuthConfig) -> Result<()> {
    match (auth.source, auth.kind) {
        (CredentialSource::Env, CredentialKind::ApiKey | CredentialKind::BearerToken) => {
            if auth.env.as_deref().unwrap_or_default().trim().is_empty() {
                return Err(anyhow!(
                    "provider {} env auth requires auth.env",
                    provider_id.as_str()
                ));
            }
        }
        (CredentialSource::ExternalCli, CredentialKind::SessionToken) => {
            if auth
                .external
                .as_deref()
                .unwrap_or_default()
                .trim()
                .is_empty()
            {
                return Err(anyhow!(
                    "provider {} external_cli auth requires auth.external",
                    provider_id.as_str()
                ));
            }
        }
        (
            CredentialSource::AuthProfile,
            CredentialKind::ApiKey
            | CredentialKind::BearerToken
            | CredentialKind::OAuth
            | CredentialKind::SessionToken,
        ) => {
            let profile = auth.profile.as_deref().ok_or_else(|| {
                anyhow!(
                    "provider {} credential_profile auth requires auth.profile",
                    provider_id.as_str()
                )
            })?;
            if profile.trim().is_empty() {
                return Err(anyhow!(
                    "provider {} credential_profile auth requires auth.profile",
                    provider_id.as_str()
                ));
            }
            normalize_credential_profile_id(profile).with_context(|| {
                format!(
                    "provider {} credential_profile auth has invalid auth.profile",
                    provider_id.as_str()
                )
            })?;
        }
        (CredentialSource::None, CredentialKind::None) => {}
        _ => {
            return Err(anyhow!(
                "provider {} unsupported auth contract {}+{}",
                provider_id.as_str(),
                auth.source.as_str(),
                auth.kind.as_str()
            ));
        }
    }
    Ok(())
}

pub fn provider_registry_for_tests(
    openai_key: Option<&str>,
    anthropic_token: Option<&str>,
    codex_home: PathBuf,
) -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let openai_codex = ProviderId::openai_codex();
    registry.insert(
        openai_codex.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: openai_codex.clone(),
            route_provider: openai_codex,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: "https://chatgpt.com/backend-api/codex".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
                external: Some("codex_cli".into()),
                client_id: None,
            },
            credential: None,
            credential_store_path: None,
            codex_home: Some(codex_home),
            originator: Some("codex_cli_rs".into()),
            reasoning_effort: Some("low".into()),
            context_management: Default::default(),
            builtin_web_search: Some(openai_codex_builtin_web_search_config()),
        },
    );
    let openai = ProviderId::openai();
    registry.insert(
        openai.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: openai.clone(),
            route_provider: openai,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: openai_key.map(ToString::to_string),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(openai_builtin_web_search_config()),
        },
    );
    let anthropic = ProviderId::anthropic();
    registry.insert(
        anthropic.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: anthropic.clone(),
            route_provider: anthropic,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: "https://api.anthropic.com".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
                client_id: None,
            },
            credential: anthropic_token.map(ToString::to_string),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(anthropic_builtin_web_search_config()),
        },
    );
    let gemini = ProviderId::gemini();
    registry.insert(
        gemini.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: gemini.clone(),
            route_provider: gemini,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::GeminiGenerateContent,
            base_url: "https://generativelanguage.googleapis.com/v1beta".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("GEMINI_API_KEY".into()),
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
        },
    );
    let typesafe = ProviderId::parse("typesafe").expect("typesafe provider id");
    registry.insert(
        typesafe.clone(),
        ProviderRuntimeConfig {
            headers: Default::default(),
            id: typesafe.clone(),
            route_provider: typesafe,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::AiEvaluationModel,
            base_url: "https://api.typesafe.ai/v1/systemone".into(),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("TYPESAFE_API_KEY".into()),
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
        },
    );
    registry
}
