use super::*;

#[test]
fn parse_minimal_model() {
    let json = r#"{
        "test-provider": {
            "id": "test-provider",
            "models": {
                "test-model": {
                    "id": "test-model"
                }
            }
        }
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    let provider = &snapshot.providers["test-provider"];
    assert_eq!(provider.id, "test-provider");
    assert!(provider.env.is_empty());
    assert!(provider.api.is_none());
    let model = &provider.models["test-model"];
    assert_eq!(model.id, "test-model");
    assert!(model.name.is_none());
    assert!(model.reasoning.is_none());
}

#[test]
fn parse_full_model() {
    let json = r#"{
        "openai": {
            "id": "openai",
            "env": ["OPENAI_API_KEY"],
            "npm": "@ai-sdk/openai",
            "api": "https://api.openai.com/v1",
            "name": "OpenAI",
            "doc": "https://platform.openai.com/docs",
            "models": {
                "gpt-5.5": {
                    "id": "gpt-5.5",
                    "name": "GPT-5.5",
                    "description": "Default frontier GPT",
                    "family": "gpt",
                    "attachment": true,
                    "reasoning": true,
                    "reasoning_options": [{"type": "effort", "values": ["none", "low", "high"]}],
                    "tool_call": true,
                    "structured_output": true,
                    "temperature": true,
                    "knowledge": "2025-12-01",
                    "release_date": "2026-04-23",
                    "last_updated": "2026-04-23",
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "open_weights": false,
                    "limit": {"context": 1050000, "output": 128000},
                    "cost": {"input": 5, "output": 30, "cache_read": 0.5}
                }
            }
        }
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    let model = &snapshot.providers["openai"].models["gpt-5.5"];
    assert_eq!(model.name.as_deref(), Some("GPT-5.5"));
    assert_eq!(model.reasoning, Some(true));
    assert_eq!(model.reasoning_options.len(), 1);
    assert_eq!(model.reasoning_options[0].r#type.as_deref(), Some("effort"));
    assert_eq!(
        model.reasoning_options[0].values,
        vec!["none", "low", "high"]
    );
    assert_eq!(model.limit.as_ref().unwrap().context, Some(1050000));
    assert_eq!(
        model.modalities.as_ref().unwrap().input,
        vec!["text", "image"]
    );
}

#[test]
fn parses_both_interleaved_shapes() {
    let json = r#"{
        "bool-provider": {
            "id": "bool-provider",
            "models": {
                "m": {"id": "m", "interleaved": true}
            }
        },
        "obj-provider": {
            "id": "obj-provider",
            "models": {
                "m": {"id": "m", "interleaved": {"field": "reasoning_content"}}
            }
        }
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    assert_eq!(
        snapshot.providers["bool-provider"].models["m"].interleaved,
        Some(ModelsDevInterleaved {
            enabled: Some(true),
            field: None
        })
    );
    assert_eq!(
        snapshot.providers["obj-provider"].models["m"].interleaved,
        Some(ModelsDevInterleaved {
            enabled: None,
            field: Some("reasoning_content".to_string())
        })
    );
}

#[test]
fn ignores_unknown_fields() {
    let json = r#"{
        "test": {
            "id": "test",
            "future_field": "ignored",
            "models": {
                "m": {"id": "m", "another_future_field": 42}
            }
        }
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    assert_eq!(snapshot.providers["test"].models["m"].id, "m");
}

#[test]
fn ignores_unknown_top_level_scalar_fields() {
    let json = r#"{
        "schema_version": 2,
        "generated_at": "2026-08-31T00:00:00Z",
        "test": {
            "id": "test",
            "models": {}
        }
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    assert_eq!(snapshot.providers.len(), 1);
    assert_eq!(snapshot.providers["test"].id, "test");
}
