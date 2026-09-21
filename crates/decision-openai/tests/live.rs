use decision_core::{DecisionContext, DecisionOutcome, DecisionProvider, DecisionRequest};
use decision_openai::{OpenAiConfig, OpenAiProvider};
use serde_json::{json, Value};
use std::{collections::BTreeMap, env, time::Duration};

fn live_request() -> DecisionRequest<Value, Value> {
    DecisionRequest {
        request_id: "live-decision-openai".into(),
        input: json!({"text": "route this message to the best matching candidate"}),
        candidates: vec![json!("support"), json!("sales")],
        schema: "Select exactly one candidate and return a DecisionResponse JSON object.".into(),
        schema_version: "1".into(),
        metadata: BTreeMap::new(),
        deadline_ms: Some(30_000),
    }
}

fn required_env(names: &[&str]) -> Option<String> {
    match names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
    {
        Some(value) => Some(value),
        None => {
            eprintln!(
                "skipping live decision-openai test: none of {} is configured",
                names.join(", ")
            );
            None
        }
    }
}

async fn run_live(
    endpoint: String,
    model: String,
    api_key: String,
    request_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let provider = OpenAiProvider::new(
        OpenAiConfig::new(endpoint, model)
            .with_api_key(api_key)
            .with_timeout(Duration::from_secs(30))
            .with_max_tokens(128),
    )?;

    let mut request = live_request();
    request.request_id = request_id.into();
    let response = provider
        .decide(
            request,
            DecisionContext::with_timeout(Duration::from_secs(30)),
        )
        .await?;
    response.validate("1")?;
    assert!(!response.provenance.provider.is_empty());
    assert!(matches!(
        response.outcome,
        DecisionOutcome::Select { .. } | DecisionOutcome::Rank { .. }
    ));
    Ok(())
}

#[tokio::test]
async fn live_openai_compatible_decision_returns_valid_response(
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(api_key) = required_env(&["HOLON_LIVE_DECISION_OPENAI_API_KEY"]) else {
        return Ok(());
    };
    let endpoint = env::var("HOLON_LIVE_DECISION_OPENAI_ENDPOINT")
        .unwrap_or_else(|_| "https://api.openai.com/v1".into());
    let model =
        env::var("HOLON_LIVE_DECISION_OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
    run_live(endpoint, model, api_key, "live-decision-openai").await
}

#[tokio::test]
async fn live_openrouter_openai_compatible_decision_returns_valid_response(
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(api_key) = required_env(&[
        "HOLON_LIVE_DECISION_OPENROUTER_API_KEY",
        "OPENROUTER_API_KEY",
    ]) else {
        return Ok(());
    };
    let endpoint = env::var("HOLON_LIVE_DECISION_OPENROUTER_ENDPOINT")
        .unwrap_or_else(|_| "https://openrouter.ai/api/v1".into());
    let model = env::var("HOLON_LIVE_DECISION_OPENROUTER_MODEL")
        .unwrap_or_else(|_| "openai/gpt-4o-mini".into());
    run_live(endpoint, model, api_key, "live-decision-openrouter").await
}
