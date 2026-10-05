use anyhow::Result;
use holon::{
    config::{AppConfig, ProviderId},
    provider::{AgentProvider, OpenAiProvider, ProviderGenerateImageRequest},
};

fn live_xai_image_model() -> String {
    std::env::var("HOLON_LIVE_XAI_IMAGE_MODEL").unwrap_or_else(|_| "grok-imagine-image-2.0".into())
}

#[tokio::test]
#[ignore = "requires configured xAI credentials and network access"]
async fn live_xai_grok_imagine_generates_image_with_openai_images_api() -> Result<()> {
    let config = AppConfig::load()?;
    let provider_id = ProviderId::parse("xai")?;
    let provider_config = config
        .providers
        .get(&provider_id)
        .ok_or_else(|| anyhow::anyhow!("missing xai provider config"))?;
    let model = live_xai_image_model();
    let provider = OpenAiProvider::from_runtime_config(
        provider_config,
        &model,
        config.runtime_max_output_tokens,
        &config.home_dir,
    )?;
    let output = provider
        .generate_image(ProviderGenerateImageRequest {
            prompt: "Create a simple flat icon of a red kite on a white background.".into(),
            size: Some("1024x1024".into()),
            background: None,
            output_format: None,
        })
        .await?;

    assert_eq!(output.provider.as_str(), "xai");
    assert_eq!(output.model, model);
    assert_eq!(output.images.len(), 1);
    assert!(
        !output.images[0].bytes.is_empty(),
        "expected non-empty generated image bytes"
    );
    assert!(
        output.images[0]
            .mime
            .as_deref()
            .is_some_and(|mime| mime.starts_with("image/")),
        "expected xAI Grok Imagine to report a concrete media type"
    );
    Ok(())
}
