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
fn image_contract_endpoint_and_transport_intersection() {
    let route = ModelRouteRef::parse("openai@default/gpt-image-2").unwrap();
    let mut runtime = ProviderRuntimeConfig {
        headers: Default::default(),
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
