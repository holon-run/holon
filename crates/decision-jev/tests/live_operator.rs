use decision_core::{DecisionContext, DecisionOutcome, DecisionProvider, DecisionRequest};
use decision_jev::{JevConfig, JevProvider};
use serde_json::json;
use std::{collections::BTreeMap, env};

fn live_request() -> DecisionRequest<serde_json::Value, serde_json::Value> {
    DecisionRequest {
        request_id: "live-decision-jev-official".into(),
        input: json!({"text": "route this message to the best matching candidate"}),
        candidates: vec![json!("support"), json!("sales")],
        schema: "Select exactly one candidate.".into(),
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
                "skipping live decision-jev test: none of {} is configured",
                names.join(", ")
            );
            None
        }
    }
}

#[tokio::test]
async fn live_jev_official_decision_returns_select() -> Result<(), Box<dyn std::error::Error>> {
    let Some(api_key) = required_env(&["HOLON_LIVE_DECISION_JEV_API_KEY"]) else {
        return Ok(());
    };
    let endpoint = env::var("HOLON_LIVE_DECISION_JEV_ENDPOINT")
        .unwrap_or_else(|_| "https://api.typesafe.ai/v1/systemone".into());
    let model = env::var("HOLON_LIVE_DECISION_JEV_MODEL").unwrap_or_else(|_| "jev-latest".into());
    let provider = JevProvider::new(
        JevConfig::new(endpoint)
            .with_model(model)
            .with_api_key(api_key),
    )?;
    let result = provider
        .decide(live_request(), DecisionContext::default())
        .await?;
    assert!(matches!(result.outcome, DecisionOutcome::Select { .. }));
    Ok(())
}
