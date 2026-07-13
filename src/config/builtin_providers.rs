use super::*;

pub fn validate_provider_config(
    provider_id: &ProviderId,
    provider_config: &ProviderConfigFile,
) -> Result<()> {
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
    Err(unknown_config_key(key))
}

pub(crate) fn unset_provider_config_key(config: &mut HolonConfigFile, key: &str) -> Result<()> {
    if unset_provider_endpoint_config_key(config, key)? {
        return Ok(());
    }
    if unset_provider_plan_config_key(config, key)? {
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
    for suffix in [".auth.env", ".auth.profile", ".auth.external"] {
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
                    _ => {}
                }
            }
            return Ok(true);
        }
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

pub(crate) fn built_in_provider_default_config_with_settings(
    provider_id: &ProviderId,
    settings_env: &HashMap<String, String>,
) -> Result<Option<ProviderConfigFile>> {
    let catalog = BuiltInProviderCatalog::with_settings(settings_env)?;
    Ok(catalog
        .legacy_runtime(provider_id)
        .map(|provider| ProviderConfigFile {
            transport: provider.transport,
            base_url: provider.base_url.clone(),
            auth: ProviderAuthConfig::default(),
            reasoning_effort: None,
            builtin_web_search: None,
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

pub fn provider_config_views(config: &AppConfig) -> Vec<ProviderConfigView> {
    config
        .providers
        .values()
        .map(|provider| provider_config_view(config, provider))
        .collect()
}

pub fn provider_config_view(
    config: &AppConfig,
    provider: &ProviderRuntimeConfig,
) -> ProviderConfigView {
    ProviderConfigView {
        id: provider.id.as_str().to_string(),
        transport: provider.transport.as_str().to_string(),
        base_url: provider.base_url.clone(),
        auth: ProviderAuthView {
            source: provider.auth.source.as_str().to_string(),
            kind: provider.auth.kind.as_str().to_string(),
            env: provider.auth.env.clone(),
            profile: provider.auth.profile.clone(),
            external: provider.auth.external.clone(),
        },
        credential_configured: provider.has_configured_credential()
            || matches!(provider.auth.source, CredentialSource::None),
        configured_in_config: config.stored_config.providers.contains_key(&provider.id),
    }
}

pub(crate) fn resolve_provider_registry(
    stored_config: &HolonConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<ProviderRegistry> {
    let mut registry = built_in_provider_registry_with_settings(settings_env)?;
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
                settings_env,
                credential_store,
            )?;
            runtime.id = alias.clone();
            registry.insert(alias, runtime);
        }
    }
    for provider in registry.values_mut() {
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
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
) -> Result<ProviderRuntimeConfig> {
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

pub(crate) fn built_in_provider_registry_with_settings(
    settings_env: &HashMap<String, String>,
) -> Result<ProviderRegistry> {
    Ok(BuiltInProviderCatalog::with_settings(settings_env)?.into_legacy_registry())
}

pub(crate) fn resolved_provider_endpoint_config(
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

pub(crate) fn built_in_provider_endpoint_identity(
    legacy_provider: &ProviderId,
) -> Result<(ProviderId, ProviderEndpointId)> {
    let (provider, endpoint) = match legacy_provider.as_str() {
        "byteplus-coding" => ("byteplus", "coding"),
        "dashscope-token-plan" => ("dashscope", "token-plan"),
        "dashscope-coding-plan" => ("dashscope", "coding-plan"),
        "opencode-go-messages" => ("opencode-go", "messages"),
        "stepfun-plan" => ("stepfun", "plan"),
        "tencent-tokenhub-messages" => ("tencent-tokenhub", "messages"),
        "volcengine-coding" => ("volcengine", "coding"),
        "volcengine-agent" => ("volcengine", "plan"),
        "volcengine-image-openai" => ("volcengine", "plan"),
        "xiaomi-token-plan" => ("xiaomi", "token-plan"),
        _ => {
            return Ok((
                legacy_provider.clone(),
                ProviderEndpointId::default_endpoint(),
            ));
        }
    };
    Ok((
        ProviderId::parse(provider)?,
        ProviderEndpointId::parse(endpoint)?,
    ))
}

pub(crate) fn populate_built_in_provider_catalog(
    catalog: &mut BuiltInProviderCatalog,
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let openai_codex = ProviderId::openai_codex();
    let openai_codex_reasoning_effort = env::var("HOLON_OPENAI_CODEX_REASONING_EFFORT")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| Some("low".to_string()))
        .map(|value| validate_openai_reasoning_effort(&value).map(|_| value))
        .transpose()?;
    catalog.insert_endpoint(
        openai_codex.clone(),
        ProviderEndpointId::default_endpoint(),
        openai_codex.clone(),
        ProviderRuntimeConfig {
            id: openai_codex.clone(),
            route_provider: openai_codex,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiCodexResponses,
            base_url: env::var("HOLON_OPENAI_CODEX_BASE_URL")
                .unwrap_or_else(|_| "https://chatgpt.com/backend-api/codex".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::AuthProfile,
                kind: CredentialKind::OAuth,
                env: None,
                profile: Some(OPENAI_CODEX_CREDENTIAL_PROFILE.into()),
                external: Some("codex_cli".into()),
            },
            credential: None,
            credential_store_path: None,
            codex_home: Some(
                env::var("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| default_codex_home()),
            ),
            originator: Some("codex_cli_rs".into()),
            reasoning_effort: openai_codex_reasoning_effort,
            context_management: Default::default(),
            builtin_web_search: Some(openai_codex_builtin_web_search_config()),
        },
    );
    let openai = ProviderId::openai();
    catalog.insert_endpoint(
        openai.clone(),
        ProviderEndpointId::default_endpoint(),
        openai.clone(),
        ProviderRuntimeConfig {
            id: openai.clone(),
            route_provider: openai,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: env::var("HOLON_OPENAI_BASE_URL")
                .ok()
                .or_else(|| env::var("OPENAI_BASE_URL").ok())
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("OPENAI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: env::var("OPENAI_API_KEY").ok(),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: Some(openai_builtin_web_search_config()),
        },
    );
    let anthropic = ProviderId::anthropic();
    catalog.insert_endpoint(
        anthropic.clone(),
        ProviderEndpointId::default_endpoint(),
        anthropic.clone(),
        ProviderRuntimeConfig {
            id: anthropic.clone(),
            route_provider: anthropic,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: get_config_value("ANTHROPIC_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://api.anthropic.com".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("ANTHROPIC_AUTH_TOKEN".into()),
                profile: None,
                external: None,
            },
            credential: get_config_value("ANTHROPIC_AUTH_TOKEN", None, settings_env),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: resolve_anthropic_context_management_config()?,
            builtin_web_search: Some(anthropic_builtin_web_search_config()),
        },
    );
    let gemini = ProviderId::gemini();
    catalog.insert_endpoint(
        gemini.clone(),
        ProviderEndpointId::default_endpoint(),
        gemini.clone(),
        ProviderRuntimeConfig {
            id: gemini.clone(),
            route_provider: gemini,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::GeminiGenerateContent,
            base_url: get_config_value("HOLON_GEMINI_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://generativelanguage.googleapis.com/v1beta".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind: CredentialKind::ApiKey,
                env: Some("GEMINI_API_KEY".into()),
                profile: None,
                external: None,
            },
            credential: get_config_value("GEMINI_API_KEY", None, settings_env),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        },
    );
    insert_openai_compatible_provider(
        catalog,
        "arcee",
        "https://api.arcee.ai/v1",
        &["ARCEE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "byteplus",
        "https://ark.ap-southeast.bytepluses.com/api/v3",
        &["BYTEPLUS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "byteplus-coding",
        "https://ark.ap-southeast.bytepluses.com/api/coding/v3",
        &["BYTEPLUS_CODING_API_KEY", "BYTEPLUS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "chutes",
        "https://llm.chutes.ai/v1",
        &["CHUTES_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "dashscope",
        "https://dashscope.aliyuncs.com/apps/anthropic",
        &["DASHSCOPE_API_KEY", "QWEN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "dashscope-token-plan",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic",
        &["DASHSCOPE_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "dashscope-coding-plan",
        "https://coding.dashscope.aliyuncs.com/apps/anthropic",
        &["DASHSCOPE_CODING_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "deepseek",
        "https://api.deepseek.com/anthropic",
        &["DEEPSEEK_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "fireworks",
        "https://api.fireworks.ai/inference/v1",
        &["FIREWORKS_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "huggingface",
        "https://router.huggingface.co/v1",
        &["HUGGINGFACE_API_KEY", "HF_TOKEN"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "kilocode",
        "https://api.kilo.ai/api/gateway",
        &["KILOCODE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "litellm",
        "http://localhost:4000",
        &["LITELLM_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "mistral",
        "https://api.mistral.ai/v1",
        &["MISTRAL_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "moonshot",
        "https://api.moonshot.ai/v1",
        &["MOONSHOT_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "nearai",
        "https://cloud-api.near.ai/v1",
        &["NEARAI_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "nvidia",
        "https://integrate.api.nvidia.com/v1",
        &["NVIDIA_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "opencode-go",
        "https://opencode.ai/zen/go/v1",
        &["OPENCODE_GO_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "opencode-go-messages",
        "https://opencode.ai/zen/go/v1",
        &["OPENCODE_GO_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "openrouter",
        "https://openrouter.ai/api/v1",
        &["OPENROUTER_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "qianfan",
        "https://qianfan.baidubce.com/v2",
        &["QIANFAN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "stepfun",
        "https://api.stepfun.com/v1",
        &["STEPFUN_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "stepfun-plan",
        "https://api.stepfun.com/step_plan/v1",
        &["STEPFUN_PLAN_API_KEY", "STEPFUN_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "synthetic",
        "https://api.synthetic.new/anthropic",
        &["SYNTHETIC_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "tencent-tokenhub",
        "https://tokenhub.tencentmaas.com/v1",
        &["TOKENHUB_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "tencent-tokenhub-messages",
        "https://tokenhub.tencentmaas.com",
        &["TOKENHUB_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "together",
        "https://api.together.xyz/v1",
        &["TOGETHER_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "venice",
        "https://api.venice.ai/api/v1",
        &["VENICE_API_KEY"],
        settings_env,
    )?;
    insert_openai_compatible_provider(
        catalog,
        "vllm",
        "http://127.0.0.1:8000/v1",
        &[],
        settings_env,
    )?;
    // Standard tier — OpenAI Responses at /api/v3 (not the non-existent /api/compatible).
    insert_builtin_http_provider(
        catalog,
        "volcengine",
        ProviderTransportKind::OpenAiResponses,
        "https://ark.cn-beijing.volces.com/api/v3",
        &["VOLCENGINE_API_KEY"],
        settings_env,
    )?;
    // Coding Plan tier — OpenAI Responses at /api/coding/v3 (independent plan, isolated key).
    insert_builtin_http_provider(
        catalog,
        "volcengine-coding",
        ProviderTransportKind::OpenAiResponses,
        "https://ark.cn-beijing.volces.com/api/coding/v3",
        &["VOLCENGINE_CODING_API_KEY"],
        settings_env,
    )?;
    // Agent Plan tier — OpenAI Responses at /api/plan/v3 (text + image generation, isolated key, no cross-tier fallback).
    insert_builtin_http_provider(
        catalog,
        "volcengine-agent",
        ProviderTransportKind::OpenAiResponses,
        "https://ark.cn-beijing.volces.com/api/plan/v3",
        &[
            "VOLCENGINE_AGENT_API_KEY",
            "VOLCENGINE_IMAGE_OPENAI_API_KEY",
        ],
        settings_env,
    )?;
    insert_builtin_http_provider(
        catalog,
        "xiaomi",
        ProviderTransportKind::OpenAiResponses,
        "https://api.xiaomimimo.com/v1",
        &["XIAOMI_API_KEY"],
        settings_env,
    )?;
    insert_builtin_http_provider(
        catalog,
        "xiaomi-token-plan",
        ProviderTransportKind::OpenAiResponses,
        "https://token-plan-cn.xiaomimimo.com/v1",
        &["XIAOMI_TOKEN_PLAN_API_KEY"],
        settings_env,
    )?;
    insert_builtin_http_provider(
        catalog,
        "xai",
        ProviderTransportKind::OpenAiResponses,
        "https://api.x.ai/v1",
        &["XAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "zai",
        "https://api.z.ai/api/anthropic",
        &["ZAI_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "bigmodel",
        "https://open.bigmodel.cn/api/anthropic",
        &["BIGMODEL_API_KEY"],
        settings_env,
    )?;
    insert_anthropic_compatible_provider(
        catalog,
        "minimax",
        "https://api.minimax.io/anthropic",
        &["MINIMAX_API_KEY"],
        settings_env,
    )?;
    insert_vercel_ai_gateway_provider(catalog, settings_env)?;
    Ok(())
}

/// Public entry point for tools that need the built-in provider registry (e.g. docgen).
pub fn built_in_provider_registry() -> Result<ProviderRegistry> {
    let settings_env = load_settings_env()?;
    built_in_provider_registry_with_settings(&settings_env)
}

pub(crate) fn insert_openai_compatible_provider(
    catalog: &mut BuiltInProviderCatalog,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider(
        catalog,
        provider,
        ProviderTransportKind::OpenAiChatCompletions,
        default_base_url,
        env_names,
        settings_env,
    )
}

/// Insert an Anthropic-compatible provider using Claude Code-like prompt-cache
/// lowering while avoiding implicit Claude-specific beta injection. Operators can
/// still override cache strategy and betas explicitly with env config.
pub(crate) fn insert_anthropic_compatible_provider(
    catalog: &mut BuiltInProviderCatalog,
    provider: &str,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    let builtin_web_search = match provider {
        "zai" => Some(zai_builtin_web_search_config()),
        "bigmodel" => Some(bigmodel_builtin_web_search_config()),
        "deepseek" => Some(deepseek_builtin_web_search_config()),
        _ => None,
    };
    insert_builtin_http_provider_with_context_management(
        catalog,
        provider,
        ProviderTransportKind::AnthropicMessages,
        default_base_url,
        env_names,
        settings_env,
        resolve_anthropic_compatible_context_management_config()?,
        builtin_web_search,
    )
}

pub(crate) fn insert_vercel_ai_gateway_provider(
    catalog: &mut BuiltInProviderCatalog,
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
    let id = ProviderId::parse("vercel-ai-gateway")?;
    catalog.insert_endpoint(
        id.clone(),
        ProviderEndpointId::default_endpoint(),
        id.clone(),
        ProviderRuntimeConfig {
            id: id.clone(),
            route_provider: id,
            route_endpoint: ProviderEndpointId::default_endpoint(),
            transport: ProviderTransportKind::AnthropicMessages,
            base_url: get_config_value("HOLON_VERCEL_AI_GATEWAY_BASE_URL", None, settings_env)
                .unwrap_or_else(|| "https://ai-gateway.vercel.sh".to_string()),
            auth: ProviderAuthConfig {
                source: CredentialSource::Env,
                kind,
                env: Some(env_name),
                profile: None,
                external: None,
            },
            credential,
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management,
            builtin_web_search: None,
        },
    );
    Ok(())
}

pub(crate) fn insert_builtin_http_provider(
    catalog: &mut BuiltInProviderCatalog,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
) -> Result<()> {
    insert_builtin_http_provider_with_context_management(
        catalog,
        provider,
        transport,
        default_base_url,
        env_names,
        settings_env,
        Default::default(),
        None,
    )
}

pub(crate) fn insert_builtin_http_provider_with_context_management(
    catalog: &mut BuiltInProviderCatalog,
    provider: &str,
    transport: ProviderTransportKind,
    default_base_url: &str,
    env_names: &[&str],
    settings_env: &HashMap<String, String>,
    context_management: AnthropicContextManagementConfig,
    builtin_web_search: Option<ProviderBuiltinWebSearchConfig>,
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
                })
                .unwrap_or_default(),
            credential: credential.map(|resolution| resolution.value),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: if provider == "xai" {
                Some("medium".into())
            } else {
                None
            },
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
        kind: ProviderNativeWebSearchKind::Anthropic,
        advertised_tool_type: "web_search_20250305".to_string(),
        backend_kind: "deepseek_web_search".to_string(),
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

pub(crate) fn materialize_provider_config(
    id: ProviderId,
    provider_config: ProviderConfigFile,
    settings_env: &HashMap<String, String>,
    credential_store: &CredentialStoreFile,
    built_in: Option<ProviderRuntimeConfig>,
) -> Result<ProviderRuntimeConfig> {
    let effective_provider_config = provider_config_for_default_endpoint(&provider_config);
    validate_provider_auth(&id, &effective_provider_config.auth)?;
    let credential = resolve_provider_credential(
        &effective_provider_config.auth,
        settings_env,
        credential_store,
    )?;
    let mut runtime = built_in.unwrap_or_else(|| ProviderRuntimeConfig {
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
        validate_openai_reasoning_effort(reasoning_effort)?;
    }
    validate_provider_builtin_web_search(&id, &effective_provider_config)?;
    runtime.id = id;
    runtime.transport = effective_provider_config.transport;
    runtime.base_url = effective_provider_config.base_url;
    runtime.auth = effective_provider_config.auth;
    runtime.credential = credential;
    if provider_config.reasoning_effort.is_some() {
        runtime.reasoning_effort = provider_config.reasoning_effort;
    }
    if let Some(builtin_web_search) = provider_config.builtin_web_search {
        runtime.builtin_web_search = builtin_web_search.enabled.then_some(builtin_web_search);
    }
    Ok(runtime)
}

pub(crate) fn validate_openai_reasoning_effort(value: &str) -> Result<()> {
    match value {
        "low" | "medium" | "high" | "xhigh" => Ok(()),
        _ => Err(anyhow!(
            "invalid OpenAI Codex reasoning_effort '{value}'; must be one of low, medium, high, xhigh"
        )),
    }
}

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

pub(crate) fn validate_provider_auth(
    provider_id: &ProviderId,
    auth: &ProviderAuthConfig,
) -> Result<()> {
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
