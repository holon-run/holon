//! Operation-scoped image contracts shared by discovery, preflight and wire lowering.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::{ModelRouteRef, ProviderRuntimeConfig, ProviderTransportKind};

pub const IMAGE_GENERATION_CONTRACT_VERSION: &str = "image-generation-v1";

pub(crate) async fn generate_image_with_shared_quota(
    provider: &dyn super::AgentProvider,
    request: super::ProviderGenerateImageRequest,
) -> anyhow::Result<super::ProviderGenerateImageResponse> {
    let permit = match provider.quota_identity() {
        Some(identity) => Some(
            super::quota::ProviderQuotaCoordinator::global()
                .acquire(identity)
                .await,
        ),
        None => None,
    };
    // Hold the shared account gate for this single call; never retry a paid generation.
    let result = provider.generate_image(request).await;
    if let (Some(permit), Err(error)) = (&permit, &result) {
        if super::classify_provider_error(error).kind
            == super::retry::ProviderFailureKind::RateLimited
        {
            permit.record_rate_limit(super::retry::provider_error_retry_after(error));
        }
    }
    result
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageParameterSupport {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug)]
pub(crate) struct GeneratedImageExecution {
    pub response: super::ProviderGenerateImageResponse,
    pub selection: Value,
    pub contract: ImageGenerationContract,
    pub parameters: ResolvedImageGenerationParameters,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ImageGenerationApi {
    OpenAiImages,
    XaiImages,
    CodexHostedTool,
    VolcengineImages,
    UnknownImages,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageParameterContract {
    pub support: ImageParameterSupport,
    pub value_type: String,
    pub allowed_values: Vec<String>,
    pub omittable: bool,
    pub default_value: Option<String>,
    pub default_source: String,
    pub default_sent: bool,
}

impl ImageParameterContract {
    fn unknown() -> Self {
        Self {
            support: ImageParameterSupport::Unknown,
            value_type: "string".into(),
            allowed_values: Vec::new(),
            omittable: true,
            default_value: None,
            default_source: "unknown".into(),
            default_sent: false,
        }
    }

    fn unsupported() -> Self {
        Self {
            support: ImageParameterSupport::Unsupported,
            ..Self::unknown()
        }
    }

    fn supported(values: &[&str], default: Option<&str>) -> Self {
        Self {
            support: ImageParameterSupport::Supported,
            allowed_values: values.iter().map(|v| (*v).into()).collect(),
            default_value: default.map(str::to_owned),
            default_source: if default.is_some() {
                "provider"
            } else {
                "unknown"
            }
            .into(),
            ..Self::unknown()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageSizeConstraints {
    pub min_pixels: u64,
    pub max_pixels: u64,
    pub max_edge: u32,
    pub edge_multiple: u32,
    pub max_aspect_ratio: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageGenerationContract {
    pub route: String,
    pub contract_version: String,
    pub api: ImageGenerationApi,
    pub image_generation: bool,
    pub size: ImageParameterContract,
    pub aspect_ratio: ImageParameterContract,
    pub resolution: ImageParameterContract,
    pub background: ImageParameterContract,
    pub native_output_format: ImageParameterContract,
    pub native_output_formats: Vec<String>,
    pub deliverable_formats: Vec<String>,
    pub default_output_format: String,
    pub size_constraints: Option<ImageSizeConstraints>,
    pub mutually_exclusive: Vec<Vec<String>>,
    pub compatibility_mappings: Value,
    pub evidence_status: String,
    pub evidence: Vec<String>,
}

/// The runtime config is required: neither provider name nor wire compatibility
/// alone establishes a public service's parameter contract.
pub fn resolve_image_generation_contract(
    route: &ModelRouteRef,
    runtime: &ProviderRuntimeConfig,
    image_generation: bool,
) -> ImageGenerationContract {
    let api = image_generation_api(
        runtime.route_provider.as_str(),
        runtime.transport,
        &runtime.base_url,
    );
    let mut contract = contract_for_api(
        api,
        &route.model,
        image_generation && runtime.transport.supports_image_generation(),
    );
    contract.route = route.as_string();
    contract
}

pub(crate) fn image_generation_api(
    provider: &str,
    transport: ProviderTransportKind,
    base_url: &str,
) -> ImageGenerationApi {
    let Ok(url) = reqwest::Url::parse(base_url) else {
        return ImageGenerationApi::UnknownImages;
    };
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return ImageGenerationApi::UnknownImages;
    }
    let path = url.path().trim_end_matches('/');
    let images_transport = matches!(
        transport,
        ProviderTransportKind::OpenAiResponses | ProviderTransportKind::OpenAiChatCompletions
    );
    match (provider, url.host_str(), transport) {
        ("openai", Some("api.openai.com"), _)
            if images_transport && matches!(path, "" | "/v1" | "/v1/images/generations") =>
        {
            ImageGenerationApi::OpenAiImages
        }
        ("xai", Some("api.x.ai"), _)
            if images_transport && matches!(path, "" | "/v1" | "/v1/images/generations") =>
        {
            ImageGenerationApi::XaiImages
        }
        (
            "openai-codex",
            Some("chatgpt.com" | "api.openai.com"),
            ProviderTransportKind::OpenAiCodexResponses,
        ) if matches!(path, "/backend-api/codex" | "" | "/v1") => {
            ImageGenerationApi::CodexHostedTool
        }
        ("volcengine", Some("ark.cn-beijing.volces.com"), _)
            if images_transport && matches!(path, "/api/v3" | "/api/v3/images/generations") =>
        {
            ImageGenerationApi::VolcengineImages
        }
        _ => ImageGenerationApi::UnknownImages,
    }
}

/// Wire adapters use this same reviewed, model-specific contract table.
pub(crate) fn contract_for_api(
    api: ImageGenerationApi,
    model: &str,
    image_generation: bool,
) -> ImageGenerationContract {
    let mut c = ImageGenerationContract {
        route: model.into(),
        contract_version: IMAGE_GENERATION_CONTRACT_VERSION.into(),
        api,
        image_generation,
        size: ImageParameterContract::unknown(),
        aspect_ratio: ImageParameterContract::unknown(),
        resolution: ImageParameterContract::unknown(),
        background: ImageParameterContract::unknown(),
        native_output_format: ImageParameterContract::unknown(),
        native_output_formats: Vec::new(),
        deliverable_formats: vec!["png".into(), "jpeg".into(), "webp".into()],
        default_output_format: "png".into(),
        size_constraints: None,
        mutually_exclusive: vec![
            vec!["size".into(), "aspect_ratio".into()],
            vec!["size".into(), "resolution".into()],
        ],
        compatibility_mappings: json!({}),
        evidence_status: "unconfirmed".into(),
        evidence: Vec::new(),
    };
    if !image_generation {
        return c;
    }
    match api {
        ImageGenerationApi::OpenAiImages
            if matches!(
                model,
                "gpt-image-1"
                    | "gpt-image-1-mini"
                    | "gpt-image-1.5"
                    | "gpt-image-2"
                    | "gpt-image-2-2026-04-21"
                    | "gpt-image-2.5-sunburst"
                    | "gpt-image-2.5-sunburst-2026-09-08"
                    | "gpt-image-2.5-flare"
                    | "gpt-image-2.5-flare-2026-09-08"
                    | "chatgpt-image-latest"
            ) =>
        {
            c.size = ImageParameterContract::supported(
                &["auto", "1024x1024", "1536x1024", "1024x1536"],
                Some("auto"),
            );
            if model.starts_with("gpt-image-2") {
                c.size_constraints = Some(ImageSizeConstraints {
                    min_pixels: 655_360,
                    max_pixels: 8_294_400,
                    max_edge: 3840,
                    edge_multiple: 16,
                    max_aspect_ratio: 3,
                });
            }
            c.background =
                ImageParameterContract::supported(&["auto", "opaque", "transparent"], Some("auto"));
            c.native_output_format =
                ImageParameterContract::supported(&["png", "jpeg", "webp"], Some("png"));
            c.aspect_ratio = ImageParameterContract::unsupported();
            c.resolution = ImageParameterContract::unsupported();
            c.evidence.push(
                "https://developers.openai.com/api/reference/resources/images/methods/generate"
                    .into(),
            );
            c.evidence
                .push("https://developers.openai.com/api/docs/guides/image-prompting".into());
        }
        ImageGenerationApi::CodexHostedTool => {
            // The outer model only qualifies through the resolved image_output
            // capability; these are options of the hosted image_generation tool.
            c.size = ImageParameterContract::supported(
                &["auto", "1024x1024", "1536x1024", "1024x1536"],
                Some("auto"),
            );
            c.background =
                ImageParameterContract::supported(&["auto", "opaque", "transparent"], Some("auto"));
            c.native_output_format =
                ImageParameterContract::supported(&["png", "jpeg", "webp"], Some("png"));
            c.aspect_ratio = ImageParameterContract::unsupported();
            c.resolution = ImageParameterContract::unsupported();
            c.evidence.push(
                "https://developers.openai.com/api/docs/guides/tools-image-generation".into(),
            );
        }
        ImageGenerationApi::XaiImages
            if matches!(
                model,
                "grok-imagine-image" | "grok-imagine-image-2.0" | "grok-imagine-image-quality"
            ) =>
        {
            c.size =
                ImageParameterContract::supported(&["1024x1024", "1536x1024", "1024x1536"], None);
            c.aspect_ratio = ImageParameterContract::supported(
                &[
                    "auto", "1:1", "16:9", "9:16", "4:3", "3:4", "3:2", "2:3", "2:1", "1:2",
                    "19.5:9", "9:19.5", "20:9", "9:20", "21:9", "5:2",
                ],
                Some("auto"),
            );
            c.resolution = ImageParameterContract::supported(&["1k", "2k"], Some("1k"));
            c.background = ImageParameterContract::unsupported();
            c.native_output_format = ImageParameterContract::unsupported();
            c.compatibility_mappings = json!({
                "1024x1024": {"aspect_ratio": "1:1", "resolution": "1k", "exact_pixels": false},
                "1536x1024": {"aspect_ratio": "3:2", "resolution": "1k", "exact_pixels": false},
                "1024x1536": {"aspect_ratio": "2:3", "resolution": "1k", "exact_pixels": false},
            });
            c.evidence
                .push("https://docs.x.ai/developers/model-capabilities/images/generation".into());
        }
        ImageGenerationApi::VolcengineImages
            if matches!(
                model,
                "doubao-seedream-5.0-lite" | "doubao-seedream-5-0-lite-260128"
            ) =>
        {
            // Pixel bounds are not inferred from earlier Seedream versions.
            c.size = ImageParameterContract::supported(&["2K", "3K", "4K"], None);
            // The current official size table is confirmed. Do not infer the
            // rest of this model's options from the OpenAI-shaped endpoint.
            c.evidence
                .push("https://docs.volcengine.com/docs/ark/image-generation-api?lang=en".into());
        }
        _ => {}
    }
    if !c.evidence.is_empty() {
        c.evidence_status = "reviewed_official_documentation".into();
    }
    c.native_output_formats = c.native_output_format.allowed_values.clone();
    c
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct ImageGenerationParameters {
    pub size: Option<String>,
    pub aspect_ratio: Option<String>,
    pub resolution: Option<String>,
    pub background: Option<String>,
    /// Final runtime delivery format, not an unconditional wire parameter.
    pub output_format: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedImageGenerationParameters {
    pub requested: ImageGenerationParameters,
    pub provider_parameters: ImageGenerationParameters,
    pub effective_parameters: Value,
    pub delivery_format: String,
    pub defaults: Value,
    pub mapping: Value,
}

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{code}: {field} on {route}: {message}")]
pub struct ImageGenerationParameterError {
    pub code: &'static str,
    pub route: String,
    pub field: String,
    pub message: String,
    pub allowed_values: Vec<String>,
    pub recovery_hint: String,
}

impl ImageGenerationContract {
    fn error(
        &self,
        code: &'static str,
        field: &str,
        message: &str,
        allowed: &[String],
    ) -> ImageGenerationParameterError {
        ImageGenerationParameterError {
            code, route: self.route.clone(), field: field.into(), message: message.into(),
            allowed_values: allowed.to_vec(),
            recovery_hint: "omit the optional generation parameter or select a route with confirmed support; do not retry a paid generation to fix local delivery".into(),
        }
    }

    fn check(
        &self,
        field: &str,
        value: &Option<String>,
        c: &ImageParameterContract,
    ) -> Result<(), ImageGenerationParameterError> {
        let Some(value) = value else { return Ok(()) };
        match c.support {
            ImageParameterSupport::Unsupported => {
                return Err(self.error(
                    "unsupported_image_parameter",
                    field,
                    "parameter is unsupported",
                    &c.allowed_values,
                ))
            }
            ImageParameterSupport::Unknown => {
                return Err(self.error(
                    "image_parameter_capability_unknown",
                    field,
                    "parameter support is unconfirmed",
                    &c.allowed_values,
                ))
            }
            ImageParameterSupport::Supported => {}
        }
        if c.allowed_values.iter().any(|v| v == value) {
            return Ok(());
        }
        if field == "size" {
            if let (Some(constraints), Some((w, h))) =
                (&self.size_constraints, parse_image_dimensions(value))
            {
                let pixels = u64::from(w) * u64::from(h);
                if w > 0
                    && h > 0
                    && w <= constraints.max_edge
                    && h <= constraints.max_edge
                    && w % constraints.edge_multiple == 0
                    && h % constraints.edge_multiple == 0
                    && pixels >= constraints.min_pixels
                    && pixels <= constraints.max_pixels
                    && u64::from(w.max(h))
                        <= u64::from(w.min(h)) * u64::from(constraints.max_aspect_ratio)
                {
                    return Ok(());
                }
            }
        }
        Err(self.error(
            "invalid_image_parameter",
            field,
            "value violates the route's allowed values or dimension constraints",
            &c.allowed_values,
        ))
    }

    pub fn resolve_parameters(
        &self,
        p: &ImageGenerationParameters,
    ) -> Result<ResolvedImageGenerationParameters, ImageGenerationParameterError> {
        if !self.image_generation {
            return Err(self.error(
                "media_capability_mismatch",
                "model",
                "route cannot generate images",
                &[],
            ));
        }
        let target = p.output_format.as_deref().unwrap_or("png");
        if !self.deliverable_formats.iter().any(|f| f == target) {
            return Err(self.error(
                "invalid_image_parameter",
                "output_format",
                "expected png, jpeg or webp",
                &self.deliverable_formats,
            ));
        }
        if p.size.is_some() && (p.aspect_ratio.is_some() || p.resolution.is_some()) {
            return Err(self.error(
                "invalid_image_parameter",
                "size",
                "size and aspect_ratio/resolution are mutually exclusive",
                &[],
            ));
        }
        if p.background.as_deref() == Some("transparent") && target == "jpeg" {
            return Err(self.error(
                "invalid_image_parameter",
                "output_format",
                "JPEG cannot preserve a transparent background; use png or webp",
                &self.deliverable_formats,
            ));
        }
        self.check("size", &p.size, &self.size)?;
        self.check("aspect_ratio", &p.aspect_ratio, &self.aspect_ratio)?;
        self.check("resolution", &p.resolution, &self.resolution)?;
        self.check("background", &p.background, &self.background)?;
        let mut wire = p.clone();
        wire.output_format = if self.native_output_formats.iter().any(|f| f == target) {
            Some(target.into())
        } else {
            None
        };
        let mut mapping = json!(null);
        if self.api == ImageGenerationApi::XaiImages {
            if let Some(size) = &p.size {
                let mapped = &self.compatibility_mappings[size];
                wire.size = None;
                wire.aspect_ratio = mapped["aspect_ratio"].as_str().map(str::to_owned);
                wire.resolution = mapped["resolution"].as_str().map(str::to_owned);
                mapping = mapped.clone();
            }
        }
        let effective = |value: &Option<String>, field: &ImageParameterContract, source: &str| {
            json!({
                "value": value.as_ref().or(field.default_value.as_ref()),
                "source": if value.is_some() { source } else { &field.default_source },
                "sent": value.is_some(),
            })
        };
        let effective_parameters = json!({
            "size": effective(&wire.size, &self.size, "caller"),
            "aspect_ratio": effective(&wire.aspect_ratio, &self.aspect_ratio,
                if mapping.is_null() { "caller" } else { "adapter_mapping" }),
            "resolution": effective(&wire.resolution, &self.resolution,
                if mapping.is_null() { "caller" } else { "adapter_mapping" }),
            "background": effective(&wire.background, &self.background, "caller"),
            "native_output_format": effective(&wire.output_format, &self.native_output_format, "adapter"),
        });
        Ok(ResolvedImageGenerationParameters {
            requested: p.clone(),
            provider_parameters: wire,
            effective_parameters,
            delivery_format: target.into(),
            defaults: json!({
                "size": self.size, "aspect_ratio": self.aspect_ratio,
                "resolution": self.resolution, "background": self.background,
                "native_output_format": self.native_output_format,
                "delivery_format": {"value": target, "source": if p.output_format.is_some() { "caller" } else { "runtime" }},
            }),
            mapping,
        })
    }
}

pub fn parse_image_dimensions(value: &str) -> Option<(u32, u32)> {
    let (w, h) = value.split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        quota::ProviderQuotaCoordinator, AgentProvider, ProviderGenerateImageRequest,
        ProviderGenerateImageResponse, ProviderQuotaIdentity, ProviderTurnRequest,
        ProviderTurnResponse,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::time::{advance, timeout, Duration};

    #[derive(Clone, Copy)]
    enum QuotaTestOutcome {
        Success,
        RateLimited,
        Pending,
    }

    struct QuotaTestProvider {
        identity: Option<ProviderQuotaIdentity>,
        calls: AtomicUsize,
        outcome: QuotaTestOutcome,
    }

    #[async_trait::async_trait]
    impl AgentProvider for QuotaTestProvider {
        fn quota_identity(&self) -> Option<ProviderQuotaIdentity> {
            self.identity.clone()
        }

        async fn complete_turn(
            &self,
            _request: ProviderTurnRequest,
        ) -> anyhow::Result<ProviderTurnResponse> {
            anyhow::bail!("image quota tests must not run agent turns")
        }

        async fn generate_image(
            &self,
            _request: ProviderGenerateImageRequest,
        ) -> anyhow::Result<ProviderGenerateImageResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.outcome {
                QuotaTestOutcome::Success => Ok(ProviderGenerateImageResponse {
                    provider: "quota-test".into(),
                    model: "image".into(),
                    images: Vec::new(),
                }),
                QuotaTestOutcome::RateLimited => {
                    use crate::provider::retry::{
                        provider_transport_error_with_code_and_retry_after,
                        ProviderFailureClassification, ProviderFailureKind, RetryDisposition,
                    };
                    Err(provider_transport_error_with_code_and_retry_after(
                        ProviderFailureClassification {
                            kind: ProviderFailureKind::RateLimited,
                            disposition: RetryDisposition::Retryable,
                        },
                        Some("rate_limit_exceeded"),
                        Some(429),
                        None,
                        Some(Duration::from_secs(5)),
                        "scripted image rate limit",
                    ))
                }
                QuotaTestOutcome::Pending => std::future::pending().await,
            }
        }
    }

    fn quota_test_request() -> ProviderGenerateImageRequest {
        ProviderGenerateImageRequest {
            material: Default::default(),
            prompt: "test".into(),
            size: None,
            aspect_ratio: None,
            resolution: None,
            background: None,
            output_format: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn generate_image_shared_quota_gate_waits_without_generating() {
        let identity = ProviderQuotaIdentity::coarse("image-quota-test", "held");
        let held = ProviderQuotaCoordinator::global()
            .acquire(identity.clone())
            .await;
        let provider = QuotaTestProvider {
            identity: Some(identity),
            calls: AtomicUsize::new(0),
            outcome: QuotaTestOutcome::Success,
        };
        let mut call = Box::pin(generate_image_with_shared_quota(
            &provider,
            quota_test_request(),
        ));
        assert!(timeout(Duration::from_millis(1), call.as_mut())
            .await
            .is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);

        for identity in [
            None,
            Some(ProviderQuotaIdentity::coarse("image-quota-test", "other")),
        ] {
            let other = QuotaTestProvider {
                identity,
                calls: AtomicUsize::new(0),
                outcome: QuotaTestOutcome::Success,
            };
            generate_image_with_shared_quota(&other, quota_test_request())
                .await
                .unwrap();
            assert_eq!(other.calls.load(Ordering::SeqCst), 1);
        }

        drop(held);
        call.await.unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn generate_image_shared_quota_records_cooldown_without_retry() {
        let identity = ProviderQuotaIdentity::coarse("image-quota-test", "rate-limit");
        let provider = QuotaTestProvider {
            identity: Some(identity.clone()),
            calls: AtomicUsize::new(0),
            outcome: QuotaTestOutcome::RateLimited,
        };
        assert!(
            generate_image_with_shared_quota(&provider, quota_test_request())
                .await
                .is_err()
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let coordinator = ProviderQuotaCoordinator::global();
        assert!(timeout(
            Duration::from_millis(1),
            coordinator.acquire(identity.clone())
        )
        .await
        .is_err());
        advance(Duration::from_secs(5)).await;
        drop(coordinator.acquire(identity).await);
    }

    #[tokio::test(start_paused = true)]
    async fn generate_image_shared_quota_releases_permit_on_cancellation() {
        let identity = ProviderQuotaIdentity::coarse("image-quota-test", "cancel");
        let provider = QuotaTestProvider {
            identity: Some(identity.clone()),
            calls: AtomicUsize::new(0),
            outcome: QuotaTestOutcome::Pending,
        };
        assert!(timeout(
            Duration::from_millis(1),
            generate_image_with_shared_quota(&provider, quota_test_request())
        )
        .await
        .is_err());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let permit = timeout(
            Duration::from_millis(1),
            ProviderQuotaCoordinator::global().acquire(identity),
        )
        .await
        .expect("cancelled image calls must release the shared gate");
        drop(permit);
    }

    #[test]
    fn image_contract_exact_selection_never_falls_back_or_changes_defaults() {
        use crate::{
            config::{ResolvedProviderEndpointConfig, RuntimeModelCatalog},
            context::ContextConfig,
        };
        let default = ModelRouteRef::parse("openai@default/gpt-image-2").unwrap();
        let runtime = ProviderRuntimeConfig {
            id: default.provider.clone(),
            route_provider: default.provider.clone(),
            route_endpoint: default.endpoint.clone(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            auth: Default::default(),
            credential: Some("test-not-used".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        };
        let catalog = RuntimeModelCatalog {
            image_generation_model: Some(default.clone()),
            provider_endpoints: vec![
                ResolvedProviderEndpointConfig::from_provider_runtime_config(runtime),
            ],
            ..Default::default()
        };
        let context = ContextConfig::default();
        assert_eq!(
            catalog
                .select_generate_image_route_for_call(&context, None, None, None)
                .unwrap()
                .route_ref,
            default
        );
        let missing = ModelRouteRef::parse("openai@not-configured/gpt-image-2").unwrap();
        assert!(catalog
            .select_generate_image_route_for_call(&context, Some(&missing), None, None)
            .is_none());
        let not_image = ModelRouteRef::parse("openai@default/gpt-5.4").unwrap();
        assert!(catalog
            .select_generate_image_route_for_call(&context, Some(&not_image), None, None)
            .is_none());
        assert_eq!(catalog.image_generation_model, Some(default));
    }

    #[test]
    fn image_contract_endpoint_and_transport_intersection() {
        let route = ModelRouteRef::parse("openai@default/gpt-image-2").unwrap();
        let mut runtime = ProviderRuntimeConfig {
            id: route.provider.clone(),
            route_provider: route.provider.clone(),
            route_endpoint: route.endpoint.clone(),
            transport: ProviderTransportKind::OpenAiResponses,
            base_url: "https://api.openai.com/v1".into(),
            auth: Default::default(),
            credential: Some("test-not-used".into()),
            credential_store_path: None,
            codex_home: None,
            originator: None,
            reasoning_effort: None,
            context_management: Default::default(),
            builtin_web_search: None,
        };
        let c = resolve_image_generation_contract(&route, &runtime, true);
        assert_eq!(c.background.support, ImageParameterSupport::Supported);
        assert_eq!(c.route, route.as_string());
        runtime.base_url = "https://private.example/v1".into();
        let c = resolve_image_generation_contract(&route, &runtime, true);
        assert_eq!(c.background.support, ImageParameterSupport::Unknown);
        runtime.base_url = "https://api.openai.com/not-an-images-api".into();
        let c = resolve_image_generation_contract(&route, &runtime, true);
        assert_eq!(c.background.support, ImageParameterSupport::Unknown);
        runtime.base_url = "https://api.openai.com/v1".into();
        runtime.transport = ProviderTransportKind::AnthropicMessages;
        assert!(!resolve_image_generation_contract(&route, &runtime, true).image_generation);
    }

    #[test]
    fn image_contract_xai_prompt_only_and_runtime_format() {
        let c = contract_for_api(
            ImageGenerationApi::XaiImages,
            "grok-imagine-image-2.0",
            true,
        );
        let p = c
            .resolve_parameters(&ImageGenerationParameters::default())
            .unwrap();
        assert_eq!(p.delivery_format, "png");
        assert!(p.provider_parameters.background.is_none());
        assert!(p.provider_parameters.output_format.is_none());
        assert!(p.provider_parameters.aspect_ratio.is_none());
        assert_eq!(p.effective_parameters["aspect_ratio"]["value"], "auto");
        assert_eq!(p.effective_parameters["aspect_ratio"]["sent"], false);
        let error = c
            .resolve_parameters(&ImageGenerationParameters {
                background: Some("auto".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(error.code, "unsupported_image_parameter");
    }

    #[test]
    fn image_contract_size_mapping_and_conflicts() {
        let c = contract_for_api(ImageGenerationApi::XaiImages, "grok-imagine-image", true);
        let p = c
            .resolve_parameters(&ImageGenerationParameters {
                size: Some("1536x1024".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(p.provider_parameters.aspect_ratio.as_deref(), Some("3:2"));
        assert_eq!(p.mapping["exact_pixels"], false);
        assert!(c
            .resolve_parameters(&ImageGenerationParameters {
                size: Some("1536x1024".into()),
                resolution: Some("2k".into()),
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn image_contract_model_intersection_and_custom_unknown() {
        let c = contract_for_api(
            ImageGenerationApi::OpenAiImages,
            "not-a-reviewed-image-model",
            true,
        );
        assert_eq!(c.background.support, ImageParameterSupport::Unknown);
        assert!(c
            .resolve_parameters(&ImageGenerationParameters::default())
            .is_ok());
        let c = contract_for_api(ImageGenerationApi::UnknownImages, "gpt-image-2", true);
        assert!(c
            .resolve_parameters(&ImageGenerationParameters {
                size: Some("1024x1024".into()),
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn image_contract_flexible_sizes_and_alpha_preflight() {
        let c = contract_for_api(ImageGenerationApi::OpenAiImages, "gpt-image-2", true);
        assert!(c
            .resolve_parameters(&ImageGenerationParameters {
                size: Some("1536x864".into()),
                ..Default::default()
            })
            .is_ok());
        for size in ["1x1", "1537x864", "9999x9999"] {
            assert!(c
                .resolve_parameters(&ImageGenerationParameters {
                    size: Some(size.into()),
                    ..Default::default()
                })
                .is_err());
        }
        assert!(c
            .resolve_parameters(&ImageGenerationParameters {
                background: Some("transparent".into()),
                output_format: Some("jpeg".into()),
                ..Default::default()
            })
            .is_err());
    }
}
