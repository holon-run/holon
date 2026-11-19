//! Operation-scoped image contracts shared by discovery, preflight and wire lowering.
use serde_json::json;

use crate::config::{ModelRouteRef, ProviderRuntimeConfig, ProviderTransportKind};

pub const IMAGE_GENERATION_CONTRACT_VERSION: &str = "image-generation-v1";

pub async fn generate_image_with_shared_quota(
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

pub use holon_domain::{
    parse_image_dimensions, ImageGenerationApi, ImageGenerationContract,
    ImageGenerationParameterError, ImageGenerationParameters, ImageParameterContract,
    ImageParameterSupport, ImageSizeConstraints, ResolvedImageGenerationParameters,
};

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

pub fn image_generation_api(
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
pub fn contract_for_api(
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

#[cfg(test)]
mod tests;
