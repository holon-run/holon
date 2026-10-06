use holon_model_client::{
    CompletionRequest, Message, ModelClient, OpenAiCompatibleClient, OpenAiCompatibleConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = OpenAiCompatibleClient::new(OpenAiCompatibleConfig {
        api_key: Some(std::env::var("OPENAI_API_KEY")?),
        ..Default::default()
    })?;
    let request = CompletionRequest::new(
        "gpt-4o-mini",
        vec![Message::user("Say hello in one sentence.")],
    );
    let response = client.complete(&Default::default(), request).await?;
    println!("{:?}", response.message);
    Ok(())
}
