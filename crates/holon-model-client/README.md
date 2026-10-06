# holon-model-client

`holon-model-client` contains small Rust contracts for one model-provider
attempt and transport adapters that implement those contracts.

The first transport is an OpenAI Chat Completions-compatible client. The crate
does **not** select routes, retry, fall back, execute tools, maintain
transcripts, or enforce agent budgets. An embedding runtime owns those policies
and passes an opaque `CallContext` for correlation and diagnostics.

```rust,no_run
use holon_model_client::{
    CompletionRequest, Message, ModelClient, OpenAiCompatibleClient,
    OpenAiCompatibleConfig,
};

# async fn example() -> Result<(), Box<dyn std::error::Error>> {
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
# Ok(())
# }
```

Provider-native continuation, cache, reasoning, and native-search data are
represented as explicit request extensions or opaque continuation state. Route
syntax such as `provider@endpoint/model`, fallback, budget, and runtime trace
policy remain outside this crate.
