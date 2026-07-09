use anyhow::Result;
use holon::{
    config::{AppConfig, ProviderId},
    provider::{AgentProvider, OpenAiChatCompletionsProvider, ProviderGenerateImageRequest},
};

fn live_volcengine_image_model() -> String {
    std::env::var("HOLON_LIVE_VOLCENGINE_IMAGE_MODEL")
        .unwrap_or_else(|_| "doubao-seedream-5.0-lite".into())
}

#[tokio::test]
#[ignore = "requires a Volcengine Ark image generation credential profile and network access"]
async fn live_volcengine_ark_seedream_generates_image_with_openai_images_api() -> Result<()> {
    let config = AppConfig::load()?;
    let provider_id = ProviderId::parse("volcengine-agent")?;
    let provider_config = config
        .providers
        .get(&provider_id)
        .ok_or_else(|| anyhow::anyhow!("missing volcengine-agent provider config"))?;
    let model = live_volcengine_image_model();
    let provider = OpenAiChatCompletionsProvider::from_runtime_config(
        provider_config,
        &model,
        config.runtime_max_output_tokens,
        &config.home_dir,
    )?;
    let output = provider
        .generate_image(ProviderGenerateImageRequest {
            prompt: "Create a simple flat icon of a red kite on a white background.".into(),
            size: Some("1920x1920".into()),
            background: None,
            output_format: None,
        })
        .await?;

    assert_eq!(output.provider.as_str(), "volcengine-agent");
    assert_eq!(output.model, model);
    assert_eq!(output.images.len(), 1);
    assert!(
        !output.images[0].bytes.is_empty(),
        "expected non-empty generated image bytes"
    );
    Ok(())
}
