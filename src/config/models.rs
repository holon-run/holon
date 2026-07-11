use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    pub provider: ProviderId,
    pub model: String,
}

pub(crate) fn authenticated_model_route_candidates(
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Vec<ModelRouteRef> {
    authenticated_model_candidates(providers, model_overrides)
        .iter()
        .map(ModelRouteRef::from_legacy_model_ref)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderEndpointId(String);

impl ProviderEndpointId {
    pub const DEFAULT: &'static str = "default";

    pub fn default_endpoint() -> Self {
        Self(Self::DEFAULT.to_string())
    }

    pub(crate) fn parse(value: &str) -> Result<Self> {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            return Err(anyhow!("provider endpoint id must not be empty"));
        }
        if !normalized
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '_')
        {
            return Err(anyhow!(
                "invalid provider endpoint id {normalized}; expected lowercase ascii, digits, '-' or '_'"
            ));
        }
        Ok(Self(normalized))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ProviderEndpointId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProviderEndpointId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ProviderEndpointId::parse(&raw).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRouteRef {
    pub provider: ProviderId,
    pub endpoint: ProviderEndpointId,
    pub model: String,
}

impl ModelRouteRef {
    pub fn new(
        provider: ProviderId,
        endpoint: ProviderEndpointId,
        model: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            endpoint,
            model: model.into(),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("model route ref must not be empty"));
        }
        let (route, model) = trimmed.split_once('/').ok_or_else(|| {
            anyhow!("invalid model route ref {trimmed}; expected provider@endpoint/model")
        })?;
        let (provider, endpoint) = route.split_once('@').ok_or_else(|| {
            anyhow!("invalid model route ref {trimmed}; expected provider@endpoint/model")
        })?;
        let provider = ProviderId::parse(provider)?;
        let endpoint = ProviderEndpointId::parse(endpoint)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(anyhow!(
                "invalid model route ref {trimmed}; model part must not be empty"
            ));
        }
        Ok(Self::new(provider, endpoint, model))
    }

    pub fn parse_compatible(value: &str) -> Result<Self> {
        if value
            .trim()
            .split_once('/')
            .is_some_and(|(route, _)| route.contains('@'))
        {
            return Self::parse(value);
        }
        let model_ref = ModelRef::parse(value)?;
        Ok(Self::from_legacy_model_ref(&model_ref))
    }

    pub fn from_legacy_model_ref(model_ref: &ModelRef) -> Self {
        let catalog = BuiltInModelCatalog::default();
        let model_ref = catalog.canonicalize_model_ref(model_ref);
        let endpoint = catalog
            .get(&model_ref)
            .and_then(|metadata| metadata.endpoint.clone())
            .unwrap_or_else(ProviderEndpointId::default_endpoint);
        Self::new(model_ref.provider, endpoint, model_ref.model)
    }

    pub fn model_ref(&self) -> ModelRef {
        ModelRef::new(self.provider.clone(), self.model.clone())
    }

    pub fn as_string(&self) -> String {
        format!(
            "{}@{}/{}",
            self.provider.as_str(),
            self.endpoint.as_str(),
            self.model
        )
    }
}

impl Serialize for ModelRouteRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for ModelRouteRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ModelRouteRef::parse_compatible(&raw).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelRouteCapability {
    Turn,
    VisionObservation,
    ImageGeneration,
}

impl ModelRouteCapability {
    pub fn model_supports(self, policy: &ResolvedRuntimeModelPolicy) -> bool {
        match self {
            Self::Turn => true,
            Self::VisionObservation => policy.capabilities.image_input,
            Self::ImageGeneration => policy.capabilities.image_generation,
        }
    }

    pub fn transport_supports(self, transport: ProviderTransportKind) -> bool {
        match self {
            Self::Turn => true,
            Self::VisionObservation => transport.supports_view_image_observation_generation(),
            Self::ImageGeneration => transport.supports_image_generation(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProviderEndpointConfig {
    pub provider: ProviderId,
    pub endpoint: ProviderEndpointId,
    pub runtime_config: ProviderRuntimeConfig,
}

impl ResolvedProviderEndpointConfig {
    pub fn from_provider_runtime_config(runtime_config: ProviderRuntimeConfig) -> Self {
        Self {
            provider: runtime_config.id.clone(),
            endpoint: ProviderEndpointId::default_endpoint(),
            runtime_config,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedModelRoute {
    pub route_ref: ModelRouteRef,
    pub model_ref: ModelRef,
    pub endpoint: ResolvedProviderEndpointConfig,
    pub policy: ResolvedRuntimeModelPolicy,
    pub requested_capability: ModelRouteCapability,
}

impl ResolvedModelRoute {
    pub fn provider_config(&self) -> &ProviderRuntimeConfig {
        &self.endpoint.runtime_config
    }

    pub fn provider_name(&self) -> &str {
        self.endpoint.provider.as_str()
    }
}

pub(crate) fn resolve_image_generation_model(
    stored_config: &HolonConfigFile,
) -> Result<Option<ModelRouteRef>> {
    if let Ok(value) = env::var("HOLON_IMAGE_GENERATION_MODEL") {
        return parse_image_generation_model_ref(&value);
    }
    if let Some(value) = &stored_config.image_generation.default {
        return parse_image_generation_model_ref(value);
    }
    Ok(None)
}

fn parse_image_generation_model_ref(value: &str) -> Result<Option<ModelRouteRef>> {
    if value.trim().eq_ignore_ascii_case("auto") {
        Ok(None)
    } else {
        ModelRouteRef::parse_compatible(value).map(Some)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeModelCatalog {
    pub default_model: ModelRouteRef,
    pub fallback_models: Vec<ModelRouteRef>,
    pub vision_model: Option<ModelRouteRef>,
    pub image_generation_model: Option<ModelRouteRef>,
    pub vision_candidate_models: Vec<ModelRouteRef>,
    pub disable_provider_fallback: bool,
    pub provider_endpoints: HashMap<ProviderId, ResolvedProviderEndpointConfig>,
    pub built_in_catalog: BuiltInModelCatalog,
    pub discovered_models: HashMap<ModelRef, BuiltInModelMetadata>,
    pub model_overrides: HashMap<ModelRef, ModelRuntimeOverride>,
    pub unknown_model_fallback: Option<ModelRuntimeOverride>,
    pub configured_runtime_max_output_tokens: u32,
}

impl RuntimeModelCatalog {
    pub fn from_config(config: &AppConfig) -> Self {
        Self {
            default_model: config.default_model.clone(),
            fallback_models: config.fallback_models.clone(),
            vision_model: config.vision_model.clone(),
            image_generation_model: config.image_generation_model.clone(),
            vision_candidate_models: config.vision_candidate_models.clone(),
            disable_provider_fallback: config.provider_fallback_disabled(),
            provider_endpoints: config
                .providers
                .iter()
                .filter_map(|(provider, config)| {
                    resolved_provider_endpoint_config(provider.clone(), config.clone())
                        .ok()
                        .map(|endpoint| (provider.clone(), endpoint))
                })
                .collect(),
            built_in_catalog: BuiltInModelCatalog::default(),
            discovered_models: config
                .model_discovery_cache
                .models()
                .into_iter()
                .collect::<HashMap<_, _>>(),
            model_overrides: config.validated_model_overrides.clone(),
            unknown_model_fallback: config.validated_unknown_model_fallback.clone(),
            configured_runtime_max_output_tokens: config.runtime_max_output_tokens,
        }
    }

    pub fn resolve_model_route(
        &self,
        base_context_config: &ContextConfig,
        model_ref: &ModelRef,
        requested_capability: ModelRouteCapability,
    ) -> Option<ResolvedModelRoute> {
        let canonical_ref = self.built_in_catalog.canonicalize_model_ref(model_ref);
        let model_ref = &canonical_ref;

        let endpoint = self.resolve_endpoint_for_model(model_ref)?;
        let route_ref = ModelRouteRef::new(
            endpoint.provider.clone(),
            endpoint.endpoint.clone(),
            model_ref.model.clone(),
        );

        let policy = self.built_in_catalog.resolve_policy(
            model_ref,
            &self.model_overrides,
            &self.discovered_models,
            self.unknown_model_fallback.as_ref(),
            base_context_config,
            self.configured_runtime_max_output_tokens,
        );
        if !requested_capability.model_supports(&policy)
            || !requested_capability.transport_supports(endpoint.runtime_config.transport)
        {
            return None;
        }
        Some(ResolvedModelRoute {
            route_ref,
            model_ref: model_ref.clone(),
            endpoint: endpoint.clone(),
            policy,
            requested_capability,
        })
    }

    pub fn resolve_explicit_model_route(
        &self,
        base_context_config: &ContextConfig,
        route_ref: &ModelRouteRef,
        requested_capability: ModelRouteCapability,
    ) -> Option<ResolvedModelRoute> {
        let model_ref = route_ref.model_ref();
        let endpoint = self.provider_endpoints.values().find(|endpoint| {
            endpoint.provider == route_ref.provider && endpoint.endpoint == route_ref.endpoint
        })?;
        let policy = self.built_in_catalog.resolve_policy(
            &model_ref,
            &self.model_overrides,
            &self.discovered_models,
            self.unknown_model_fallback.as_ref(),
            base_context_config,
            self.configured_runtime_max_output_tokens,
        );
        if !requested_capability.model_supports(&policy)
            || !requested_capability.transport_supports(endpoint.runtime_config.transport)
        {
            return None;
        }
        Some(ResolvedModelRoute {
            route_ref: route_ref.clone(),
            model_ref,
            endpoint: endpoint.clone(),
            policy,
            requested_capability,
        })
    }

    /// Resolve the provider endpoint config for a (possibly canonical) model ref.
    /// Uses the model metadata's endpoint field when available to find the
    /// correct endpoint among multiple endpoints under the same canonical provider.
    fn resolve_endpoint_for_model(
        &self,
        model_ref: &ModelRef,
    ) -> Option<&ResolvedProviderEndpointConfig> {
        let model_endpoint = self
            .built_in_catalog
            .get(model_ref)
            .and_then(|metadata| metadata.endpoint.as_ref());
        match model_endpoint {
            // Model declares a specific non-default endpoint: find by (provider, endpoint)
            Some(endpoint) => self
                .provider_endpoints
                .values()
                .find(|e| e.provider == model_ref.provider && &e.endpoint == endpoint),
            // Default endpoint or no catalog metadata: direct key lookup
            None => self.provider_endpoints.get(&model_ref.provider),
        }
    }

    pub fn provider_chain(&self, model_override: Option<&ModelRouteRef>) -> Vec<ModelRouteRef> {
        if self.disable_provider_fallback {
            return vec![self.effective_model(model_override)];
        }
        let mut chain = Vec::new();
        if let Some(model_override) = model_override {
            chain.push(model_override.clone());
        }
        chain.push(self.default_model.clone());
        for model in &self.fallback_models {
            if !chain.iter().any(|existing| existing == model) {
                chain.push(model.clone());
            }
        }
        chain
    }

    pub fn provider_chain_for_turn(
        &self,
        model_override: Option<&ModelRouteRef>,
        pending_fallback_model: Option<&ModelRouteRef>,
    ) -> Vec<ModelRouteRef> {
        let chain = self.provider_chain(model_override);
        let Some(pending_fallback_model) = pending_fallback_model else {
            return chain;
        };
        chain
            .iter()
            .position(|model| model == pending_fallback_model)
            .map(|index| chain[index..].to_vec())
            .unwrap_or_else(|| vec![pending_fallback_model.clone()])
    }

    pub fn effective_model(&self, model_override: Option<&ModelRouteRef>) -> ModelRouteRef {
        model_override
            .cloned()
            .unwrap_or_else(|| self.default_model.clone())
    }

    pub fn resolved_model_policy(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRouteRef>,
    ) -> ResolvedRuntimeModelPolicy {
        let model_ref = self.effective_model(model_override).model_ref();
        self.built_in_catalog.resolve_policy(
            &model_ref,
            &self.model_overrides,
            &self.discovered_models,
            self.unknown_model_fallback.as_ref(),
            base_context_config,
            self.configured_runtime_max_output_tokens,
        )
    }

    pub fn resolved_context_config(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRouteRef>,
    ) -> ContextConfig {
        let model_ref = self.effective_model(model_override).model_ref();
        self.built_in_catalog
            .apply_policy(
                &model_ref,
                &self.model_overrides,
                &self.discovered_models,
                self.unknown_model_fallback.as_ref(),
                base_context_config,
                self.configured_runtime_max_output_tokens,
            )
            .0
    }

    pub fn available_models(&self) -> Vec<BuiltInModelMetadata> {
        let mut models = self.built_in_catalog.list();
        for discovered in self.discovered_models.values() {
            if !self.model_overrides.contains_key(&discovered.model_ref) {
                models.retain(|model| model.model_ref != discovered.model_ref);
                models.push(discovered.clone());
            }
        }
        for (model_ref, override_config) in &self.model_overrides {
            let base = self
                .discovered_models
                .get(model_ref)
                .or_else(|| self.built_in_catalog.get(model_ref));
            models.retain(|model| &model.model_ref != model_ref);
            models.push(BuiltInModelMetadata {
                model_ref: model_ref.clone(),
                display_name: override_config
                    .display_name
                    .clone()
                    .or_else(|| base.map(|model| model.display_name.clone()))
                    .unwrap_or_else(|| model_ref.as_string()),
                description: override_config
                    .description
                    .clone()
                    .or_else(|| base.map(|model| model.description.clone()))
                    .unwrap_or_else(|| "User-configured model metadata override.".to_string()),
                context_window_tokens: override_config
                    .context_window_tokens
                    .or_else(|| base.and_then(|model| model.context_window_tokens)),
                effective_context_window_percent: override_config
                    .effective_context_window_percent
                    .unwrap_or_else(|| {
                        base.map(|model| model.effective_context_window_percent)
                            .unwrap_or(95)
                    }),
                auto_compact_token_limit: override_config
                    .auto_compact_token_limit
                    .or_else(|| base.and_then(|model| model.auto_compact_token_limit)),
                default_max_output_tokens: override_config
                    .runtime_max_output_tokens
                    .or_else(|| base.and_then(|model| model.default_max_output_tokens)),
                max_output_tokens_upper_limit: base
                    .and_then(|model| model.max_output_tokens_upper_limit),
                default_verbosity: override_config
                    .verbosity
                    .or_else(|| base.and_then(|model| model.default_verbosity)),
                tool_output_truncation_estimated_tokens: override_config
                    .tool_output_truncation_estimated_tokens
                    .or_else(|| {
                        base.and_then(|model| model.tool_output_truncation_estimated_tokens)
                    }),
                capabilities: merged_model_capabilities(
                    base.map(|model| &model.capabilities),
                    override_config.capabilities.as_ref(),
                ),
                source: crate::model_catalog::ModelMetadataSource::ConfigOverride,
                endpoint: None,
            });
        }
        models.sort_by(|left, right| {
            left.display_name
                .cmp(&right.display_name)
                .then_with(|| left.model_ref.as_string().cmp(&right.model_ref.as_string()))
        });
        models
    }

    pub fn select_view_image_vision_model(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRouteRef>,
        pending_fallback_model: Option<&ModelRouteRef>,
    ) -> ViewImageVisionSelection {
        let chain = self.provider_chain_for_turn(model_override, pending_fallback_model);
        let primary = chain
            .first()
            .cloned()
            .unwrap_or_else(|| self.effective_model(model_override));
        if let Some(model_ref) = &self.vision_model {
            return self.select_view_image_vision_model_from_candidates(
                base_context_config,
                primary,
                vec![model_ref.clone()],
                "explicit_vision_model_supports_image_input",
                "explicit_vision_model_unavailable",
            );
        }

        let mut candidates = Vec::new();
        candidates.push(primary.clone());
        for model_ref in &self.vision_candidate_models {
            if !candidates.iter().any(|existing| existing == model_ref) {
                candidates.push(model_ref.clone());
            }
        }
        for model_ref in chain {
            if !candidates.iter().any(|existing| existing == &model_ref) {
                candidates.push(model_ref);
            }
        }

        self.select_view_image_vision_model_from_candidates(
            base_context_config,
            primary,
            candidates,
            "auto_discovered_vision_model_supports_image_input",
            "no_configured_model_supports_view_image_observation",
        )
    }

    pub(crate) fn select_view_image_vision_model_from_candidates(
        &self,
        base_context_config: &ContextConfig,
        primary: ModelRouteRef,
        model_refs: Vec<ModelRouteRef>,
        selected_adapter_reason: &str,
        unavailable_reason: &str,
    ) -> ViewImageVisionSelection {
        let mut candidates = Vec::new();
        let mut selected = None;

        for route_ref in &model_refs {
            let model_ref = route_ref.model_ref();
            let policy = self.built_in_catalog.resolve_policy(
                &model_ref,
                &self.model_overrides,
                &self.discovered_models,
                self.unknown_model_fallback.as_ref(),
                base_context_config,
                self.configured_runtime_max_output_tokens,
            );
            let image_input = policy.capabilities.image_input;
            let supported_transport = self
                .provider_endpoints
                .values()
                .find(|endpoint| {
                    endpoint.provider == route_ref.provider
                        && endpoint.endpoint == route_ref.endpoint
                })
                .is_some_and(|endpoint| {
                    ModelRouteCapability::VisionObservation
                        .transport_supports(endpoint.runtime_config.transport)
                });
            let reason = if !image_input {
                "model_lacks_image_input"
            } else if supported_transport {
                "model_advertises_image_input"
            } else {
                "provider_transport_unsupported_for_view_image_observation"
            };
            candidates.push(ViewImageVisionCandidate {
                provider: route_ref.provider.as_str().to_string(),
                model: route_ref.model.clone(),
                model_ref: route_ref.as_string(),
                image_input,
                reason: reason.to_string(),
            });
            if self
                .resolve_explicit_model_route(
                    base_context_config,
                    route_ref,
                    ModelRouteCapability::VisionObservation,
                )
                .is_some()
                && selected.is_none()
            {
                selected = Some(route_ref.clone());
            }
        }

        let Some(selected) = selected else {
            return ViewImageVisionSelection {
                selected_mode: ViewImageSelectedMode::Unavailable,
                vision_provider: None,
                vision_model: None,
                selection_reason: unavailable_reason.to_string(),
                primary_provider: Some(primary.provider.as_str().to_string()),
                primary_model: Some(primary.model),
                candidates,
            };
        };

        let primary_supports_image = selected == primary;
        ViewImageVisionSelection {
            selected_mode: if primary_supports_image {
                ViewImageSelectedMode::NativeImageWithObservation
            } else {
                ViewImageSelectedMode::VisionAdapter
            },
            vision_provider: Some(selected.provider.as_str().to_string()),
            vision_model: Some(selected.model.clone()),
            selection_reason: if primary_supports_image {
                "current_primary_model_supports_image_input"
            } else {
                selected_adapter_reason
            }
            .to_string(),
            primary_provider: Some(primary.provider.as_str().to_string()),
            primary_model: Some(primary.model),
            candidates,
        }
    }

    pub fn select_generate_image_route(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRouteRef>,
        pending_fallback_model: Option<&ModelRouteRef>,
    ) -> Option<ResolvedModelRoute> {
        let candidates = self
            .image_generation_model
            .clone()
            .map(|model_ref| vec![model_ref])
            .unwrap_or_else(|| {
                self.provider_chain_for_turn(model_override, pending_fallback_model)
            });
        candidates.into_iter().find_map(|model_ref| {
            self.resolve_explicit_model_route(
                base_context_config,
                &model_ref,
                ModelRouteCapability::ImageGeneration,
            )
        })
    }

    pub fn select_generate_image_model(
        &self,
        base_context_config: &ContextConfig,
        model_override: Option<&ModelRouteRef>,
        pending_fallback_model: Option<&ModelRouteRef>,
    ) -> Option<ModelRouteRef> {
        self.select_generate_image_route(
            base_context_config,
            model_override,
            pending_fallback_model,
        )
        .map(|route| route.route_ref)
    }
}

impl Default for RuntimeModelCatalog {
    fn default() -> Self {
        Self {
            default_model: ModelRouteRef::parse("openai@default/gpt-5.4")
                .expect("valid default model route ref"),
            fallback_models: Vec::new(),
            vision_model: None,
            image_generation_model: None,
            vision_candidate_models: Vec::new(),
            disable_provider_fallback: false,
            provider_endpoints: HashMap::new(),
            built_in_catalog: BuiltInModelCatalog::default(),
            discovered_models: HashMap::new(),
            model_overrides: HashMap::new(),
            unknown_model_fallback: None,
            configured_runtime_max_output_tokens: 8192,
        }
    }
}

pub(crate) fn merged_model_capabilities(
    base: Option<&crate::model_catalog::ModelCapabilityFlags>,
    override_config: Option<&crate::model_catalog::ModelCapabilityOverride>,
) -> crate::model_catalog::ModelCapabilityFlags {
    let mut capabilities = base.cloned().unwrap_or_default();
    if let Some(override_config) = override_config {
        if let Some(value) = override_config.parallel_tool_calls {
            capabilities.parallel_tool_calls = value;
        }
        if let Some(value) = override_config.supports_reasoning {
            capabilities.supports_reasoning = value;
        }
        if let Some(value) = override_config.image_input {
            capabilities.image_input = value;
        }
        if let Some(value) = override_config.image_generation {
            capabilities.image_generation = value;
        }
        if let Some(value) = override_config.interactive_exec {
            capabilities.interactive_exec = value;
        }
    }
    capabilities
}

impl ModelRef {
    pub fn new(provider: ProviderId, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
        }
    }

    pub fn parse(value: &str) -> Result<Self> {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            return Err(anyhow!("model ref must not be empty"));
        }
        let (provider, model) = trimmed
            .split_once('/')
            .ok_or_else(|| anyhow!("invalid model ref {trimmed}; expected provider/model"))?;
        let provider = ProviderId::parse(provider)?;
        let model = model.trim();
        if model.is_empty() {
            return Err(anyhow!(
                "invalid model ref {trimmed}; model part must not be empty"
            ));
        }
        Ok(Self {
            provider,
            model: model.to_string(),
        })
    }

    pub fn as_string(&self) -> String {
        format!("{}/{}", self.provider.as_str(), self.model)
    }
}

impl Serialize for ModelRef {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.as_string())
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        ModelRef::parse(&raw).map_err(D::Error::custom)
    }
}

pub(crate) fn resolve_model_catalog(
    stored_config: &HolonConfigFile,
) -> Result<HashMap<ModelRef, ModelRuntimeOverride>> {
    stored_config
        .models
        .catalog
        .iter()
        .map(|(model_ref, override_config)| {
            Ok((
                ModelRef::parse(model_ref)?,
                validate_model_runtime_override(override_config.clone())?,
            ))
        })
        .collect()
}

pub(crate) fn validate_optional_model_runtime_override(
    override_config: Option<ModelRuntimeOverride>,
) -> Result<Option<ModelRuntimeOverride>> {
    override_config
        .map(validate_model_runtime_override)
        .transpose()
        .map(|value| value.filter(|entry| !entry.is_empty()))
}

pub(crate) fn resolve_model_selection_for_load_mode(
    explicit_default: Option<ModelRouteRef>,
    explicit_fallbacks: Option<Vec<ModelRouteRef>>,
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
    mode: ConfigLoadMode,
) -> Result<(ModelRouteRef, Vec<ModelRouteRef>)> {
    if mode.skip_authenticated_model_resolution() {
        let default_model = explicit_default.unwrap_or_else(|| {
            ModelRouteRef::new(
                ProviderId::openai(),
                ProviderEndpointId::default_endpoint(),
                "unknown",
            )
        });
        let fallback_models = explicit_fallbacks.unwrap_or_default();
        return Ok((
            default_model.clone(),
            dedupe_fallback_models(fallback_models, &default_model),
        ));
    }

    resolve_model_selection_from_explicit(
        explicit_default,
        explicit_fallbacks,
        providers,
        model_overrides,
    )
}

pub(crate) fn resolve_model_selection_from_explicit(
    explicit_default: Option<ModelRouteRef>,
    explicit_fallbacks: Option<Vec<ModelRouteRef>>,
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Result<(ModelRouteRef, Vec<ModelRouteRef>)> {
    let auth_candidates = if explicit_default.is_none() || explicit_fallbacks.is_none() {
        authenticated_model_route_candidates(providers, model_overrides)
    } else {
        Vec::new()
    };

    let default_model = explicit_default
        .or_else(|| auth_candidates.first().cloned())
        .ok_or_else(|| {
            anyhow!(
                "no default model configured and no authenticated provider with a known model is available; set HOLON_MODEL or model.default, or configure provider credentials"
            )
        })?;
    let fallback_models = explicit_fallbacks.unwrap_or_else(|| {
        auth_candidates
            .into_iter()
            .filter(|model| model != &default_model)
            .collect()
    });

    Ok((
        default_model.clone(),
        dedupe_fallback_models(fallback_models, &default_model),
    ))
}

pub(crate) fn resolve_default_model(
    stored_config: &HolonConfigFile,
) -> Result<Option<ModelRouteRef>> {
    if let Ok(value) = env::var("HOLON_MODEL") {
        return ModelRouteRef::parse_compatible(&value).map(Some);
    }
    if let Some(value) = &stored_config.model.default {
        return ModelRouteRef::parse_compatible(value).map(Some);
    }
    Ok(None)
}

pub(crate) fn resolve_fallback_models(
    stored_config: &HolonConfigFile,
) -> Result<Option<Vec<ModelRouteRef>>> {
    if let Ok(value) = env::var("HOLON_MODEL_FALLBACKS") {
        Ok(Some(parse_model_ref_list(&value)?))
    } else if !stored_config.model.fallbacks.is_empty() {
        Ok(Some(
            stored_config
                .model
                .fallbacks
                .iter()
                .map(|value| ModelRouteRef::parse_compatible(value))
                .collect::<Result<Vec<_>>>()?,
        ))
    } else {
        Ok(None)
    }
}

pub(crate) fn resolve_vision_model(
    stored_config: &HolonConfigFile,
) -> Result<Option<ModelRouteRef>> {
    if let Ok(value) = env::var("HOLON_VISION_MODEL") {
        return ModelRouteRef::parse_compatible(&value).map(Some);
    }
    if let Some(value) = &stored_config.vision.default {
        return ModelRouteRef::parse_compatible(value).map(Some);
    }
    Ok(None)
}

pub(crate) fn authenticated_model_candidates(
    providers: &ProviderRegistry,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Vec<ModelRef> {
    let catalog = BuiltInModelCatalog::default();
    let mut provider_ids = providers
        .values()
        .filter(|provider| provider_has_usable_auth(provider))
        .map(|provider| provider.id.clone())
        .collect::<Vec<_>>();
    provider_ids.sort_by(|left, right| {
        provider_auth_priority(left)
            .cmp(&provider_auth_priority(right))
            .then_with(|| left.as_str().cmp(right.as_str()))
    });

    let mut candidates = provider_ids
        .into_iter()
        .filter_map(|provider| {
            catalog
                .preferred_model_for_provider(&provider)
                .or_else(|| preferred_override_model_for_provider(&provider, model_overrides))
        })
        .collect::<Vec<_>>();
    candidates.dedup();
    candidates
}

pub(crate) fn provider_has_usable_auth(provider: &ProviderRuntimeConfig) -> bool {
    match provider.auth.source {
        CredentialSource::Env => provider.has_configured_credential(),
        CredentialSource::AuthProfile => {
            provider.has_configured_credential()
                || (provider.id.is_openai_codex()
                    && provider.auth.profile.as_deref() == Some(OPENAI_CODEX_CREDENTIAL_PROFILE)
                    && provider
                        .codex_home
                        .as_deref()
                        .map(|home| {
                            codex_cli_auth_file_exists(home)
                                && load_codex_cli_credential(home).is_ok()
                        })
                        .unwrap_or(false))
        }
        CredentialSource::ExternalCli => {
            provider.auth.external.as_deref() == Some("codex_cli")
                && provider
                    .codex_home
                    .as_deref()
                    .map(|home| {
                        codex_cli_auth_file_exists(home) && load_codex_cli_credential(home).is_ok()
                    })
                    .unwrap_or(false)
        }
        CredentialSource::None | CredentialSource::CredentialProcess => false,
    }
}

pub(crate) fn provider_auth_priority(provider: &ProviderId) -> usize {
    match provider.as_str() {
        ProviderId::OPENAI_CODEX => 0,
        ProviderId::OPENAI => 1,
        ProviderId::ANTHROPIC => 2,
        ProviderId::GEMINI => 3,
        _ => 100,
    }
}

pub(crate) fn preferred_override_model_for_provider(
    provider: &ProviderId,
    model_overrides: &HashMap<ModelRef, ModelRuntimeOverride>,
) -> Option<ModelRef> {
    let mut models = model_overrides
        .keys()
        .filter(|model| model.provider == *provider)
        .cloned()
        .collect::<Vec<_>>();
    models.sort_by_key(ModelRef::as_string);
    models.into_iter().next()
}

pub(crate) fn dedupe_fallback_models(
    configured: Vec<ModelRouteRef>,
    default_model: &ModelRouteRef,
) -> Vec<ModelRouteRef> {
    configured
        .into_iter()
        .filter(|model| model != default_model)
        .fold(Vec::new(), |mut acc, model| {
            if !acc.iter().any(|existing| existing == &model) {
                acc.push(model);
            }
            acc
        })
}

pub(crate) fn parse_model_ref_list(raw_value: &str) -> Result<Vec<ModelRouteRef>> {
    let trimmed = raw_value.trim();
    if trimmed.starts_with('[') {
        let values: Vec<String> =
            serde_json::from_str(trimmed).context("expected a JSON string array")?;
        let parsed: Vec<ModelRouteRef> = values
            .iter()
            .map(|s| s.trim())
            .filter(|value| !value.is_empty())
            .map(ModelRouteRef::parse_compatible)
            .collect::<Result<Vec<_>>>()?;
        if parsed.is_empty() {
            return Err(anyhow!("model ref list must not be empty"));
        }
        return Ok(parsed);
    }
    let values = raw_value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ModelRouteRef::parse_compatible)
        .collect::<Result<Vec<_>>>()?;
    if values.is_empty() {
        return Err(anyhow!("model ref list must not be empty"));
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_route_ref_round_trips_model_ids_with_slashes() {
        let route_ref =
            ModelRouteRef::parse("openrouter@default/anthropic/claude-3.5-sonnet").unwrap();
        assert_eq!(route_ref.provider.as_str(), "openrouter");
        assert_eq!(route_ref.endpoint.as_str(), "default");
        assert_eq!(route_ref.model, "anthropic/claude-3.5-sonnet");
        assert_eq!(
            route_ref.as_string(),
            "openrouter@default/anthropic/claude-3.5-sonnet"
        );
    }

    #[test]
    fn compatible_model_route_ref_keeps_legacy_model_remainder() {
        let route_ref =
            ModelRouteRef::parse_compatible("openrouter/anthropic/claude-3.5-sonnet").unwrap();
        assert_eq!(route_ref.provider.as_str(), "openrouter");
        assert_eq!(route_ref.endpoint.as_str(), "default");
        assert_eq!(route_ref.model, "anthropic/claude-3.5-sonnet");
    }

    #[test]
    fn compatible_model_route_ref_upgrades_builtin_endpoint() {
        let route_ref =
            ModelRouteRef::parse_compatible("volcengine-image-openai/doubao-seedream-5.0-lite")
                .unwrap();
        assert_eq!(route_ref.provider.as_str(), "volcengine");
        assert_eq!(route_ref.endpoint.as_str(), "plan");
        assert_eq!(route_ref.model, "doubao-seedream-5.0-lite");
    }

    #[test]
    fn model_route_ref_deserializes_legacy_and_serializes_canonical() {
        let route_ref: ModelRouteRef =
            serde_json::from_str(r#""anthropic/claude-sonnet-4""#).unwrap();
        assert_eq!(
            serde_json::to_string(&route_ref).unwrap(),
            r#""anthropic@default/claude-sonnet-4""#
        );
    }

    #[test]
    fn parse_model_ref_list_json_array() {
        let refs =
            parse_model_ref_list(r#"["openai-codex/gpt-5","anthropic/claude-sonnet-4"]"#).unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].provider.as_str(), "openai-codex");
        assert_eq!(refs[1].provider.as_str(), "anthropic");
    }

    #[test]
    fn parse_model_ref_list_json_array_single() {
        let refs = parse_model_ref_list(r#"["openai-codex/gpt-5"]"#).unwrap();
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].provider.as_str(), "openai-codex");
    }

    #[test]
    fn parse_model_ref_list_comma_separated() {
        let refs = parse_model_ref_list("openai-codex/gpt-5, anthropic/claude-sonnet-4").unwrap();
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].provider.as_str(), "openai-codex");
        assert_eq!(refs[1].provider.as_str(), "anthropic");
    }

    #[test]
    fn parse_model_ref_list_empty_json_array_rejected() {
        assert!(parse_model_ref_list("[]").is_err());
    }
}

pub(crate) fn parse_model_catalog_value(
    raw_value: &str,
) -> Result<BTreeMap<String, ModelRuntimeOverride>> {
    let parsed: BTreeMap<String, ModelRuntimeOverride> =
        serde_json::from_str(raw_value).context("models.catalog expects a JSON object")?;
    let mut validated = BTreeMap::new();
    for (model_ref, override_config) in parsed {
        ModelRef::parse(&model_ref)?;
        validated.insert(model_ref, validate_model_runtime_override(override_config)?);
    }
    Ok(validated)
}

pub(crate) fn parse_optional_model_runtime_override(
    raw_value: &str,
) -> Result<Option<ModelRuntimeOverride>> {
    if raw_value.trim().eq_ignore_ascii_case("null") {
        return Ok(None);
    }
    let parsed: ModelRuntimeOverride =
        serde_json::from_str(raw_value).context("expected a JSON object or null")?;
    validate_optional_model_runtime_override(Some(parsed))
}

pub(crate) fn validate_model_runtime_override(
    override_config: ModelRuntimeOverride,
) -> Result<ModelRuntimeOverride> {
    if let Some(percent) = override_config.effective_context_window_percent {
        if percent == 0 || percent > 100 {
            return Err(anyhow!(
                "effective_context_window_percent expects an integer from 1 to 100"
            ));
        }
    }
    if let (Some(window), Some(prompt_budget)) = (
        override_config.context_window_tokens,
        override_config.prompt_budget_estimated_tokens,
    ) {
        if prompt_budget > window {
            return Err(anyhow!(
                "prompt_budget_estimated_tokens must not exceed context_window_tokens"
            ));
        }
    }
    if let (Some(trigger), Some(prompt_budget)) = (
        override_config.compaction_trigger_estimated_tokens,
        override_config.prompt_budget_estimated_tokens,
    ) {
        if trigger > prompt_budget {
            return Err(anyhow!(
                "compaction_trigger_estimated_tokens must not exceed prompt_budget_estimated_tokens"
            ));
        }
    }
    if let (Some(keep_recent), Some(trigger)) = (
        override_config.compaction_keep_recent_estimated_tokens,
        override_config.compaction_trigger_estimated_tokens,
    ) {
        if keep_recent > trigger {
            return Err(anyhow!(
                "compaction_keep_recent_estimated_tokens must not exceed compaction_trigger_estimated_tokens"
            ));
        }
    }
    if override_config.is_empty() {
        return Ok(ModelRuntimeOverride::default());
    }
    Ok(override_config)
}

pub(crate) fn ensure_unknown_model_fallback(
    config: &mut HolonConfigFile,
) -> &mut ModelRuntimeOverride {
    config
        .model
        .unknown_fallback
        .get_or_insert_with(ModelRuntimeOverride::default)
}

pub(crate) fn clear_unknown_model_fallback_field(
    config: &mut HolonConfigFile,
    clear: impl FnOnce(&mut ModelRuntimeOverride),
) {
    if let Some(value) = config.model.unknown_fallback.as_mut() {
        clear(value);
        if value.is_empty() {
            config.model.unknown_fallback = None;
        }
    }
}
