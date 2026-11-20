use serde_json::Value;

pub use holon_provider::provider::image_generation::*;

#[derive(Debug)]
pub(crate) struct GeneratedImageExecution {
    pub response: super::ProviderGenerateImageResponse,
    pub selection: Value,
    pub contract: ImageGenerationContract,
    pub parameters: ResolvedImageGenerationParameters,
}

#[cfg(all(test, holon_lib_test_shard = "c"))]
mod tests {
    use crate::config::{ModelRouteRef, ProviderRuntimeConfig, ProviderTransportKind};
    #[test]
    fn image_contract_exact_selection_never_falls_back_or_changes_defaults() {
        use crate::{
            config::{ResolvedProviderEndpointConfig, RuntimeModelCatalog},
            context::ContextConfig,
        };
        let default = ModelRouteRef::parse("openai@default/gpt-image-2").unwrap();
        let runtime = ProviderRuntimeConfig {
            headers: Default::default(),
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
}
