use super::*;
use crate::ModelMetadataSource;

fn sample_snapshot() -> ModelsDevSnapshot {
    let json = r#"{
        "openai": {
            "id": "openai",
            "models": {
                "gpt-5.5": {
                    "id": "gpt-5.5",
                    "name": "GPT-5.5",
                    "description": "Default frontier GPT",
                    "reasoning": true,
                    "reasoning_options": [{"type": "effort", "values": ["none", "low", "high"]}],
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "limit": {"context": 1050000, "output": 128000}
                },
                "gpt-4o-mini": {
                    "id": "gpt-4o-mini",
                    "name": "GPT-4o mini",
                    "reasoning": false,
                    "modalities": {"input": ["text", "image"], "output": ["text", "image"]},
                    "limit": {"context": 128000, "output": 16384}
                }
            }
        },
        "unknown-provider": {
            "id": "unknown-provider",
            "models": {
                "mystery-model": {
                    "id": "mystery-model",
                    "name": "Mystery Model"
                }
            }
        }
    }"#;
    serde_json::from_str(json).unwrap()
}

#[test]
fn gemini_carbon_keeps_the_direct_non_streaming_rest_output_limit() {
    let model: ModelsDevModel = serde_json::from_str(
        r#"{"id":"gemini-4-carbon","reasoning":true,
            "reasoning_options":[{"type":"effort","values":["low","medium","high"]}],
            "modalities":{"input":["text","image"],"output":["text"]},
            "limit":{"context":1048576,"output":32768}}"#,
    )
    .unwrap();
    let metadata = project_model(&ModelRef::parse("gemini/gemini-4-carbon").unwrap(), &model);
    assert_eq!(metadata.context_window_tokens, Some(1_048_576));
    assert_eq!(metadata.default_max_output_tokens, Some(32_768));
    assert_eq!(metadata.max_output_tokens_upper_limit, Some(32_768));
    assert_eq!(metadata.reasoning_effort_options, ["low", "medium", "high"]);
    assert!(metadata.capabilities.image_input);
    assert!(metadata.capabilities.supports_reasoning);
    assert!(!metadata.capabilities.image_generation);

    for model_ref in [
        "gemini/gemini-4-argon",
        "gemini/gemini-4-carbon-preview",
        "opencode-go/gemini-4-carbon",
    ] {
        let metadata = project_model(&ModelRef::parse(model_ref).unwrap(), &model);
        assert_eq!(metadata.default_max_output_tokens, Some(32_768));
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(32_768));
    }
}

#[test]
fn gemini_hydrogen_keeps_its_distinct_native_rest_limits_and_efforts() {
    let model: ModelsDevModel = serde_json::from_str(
        r#"{"id":"gemini-4-hydrogen","reasoning":true,
            "reasoning_options":[{"type":"effort","values":["low","medium","high","xhigh"]}],
            "modalities":{"input":["text","image","video","audio","pdf"],"output":["text"]},
            "limit":{"context":1048576,"output":65536}}"#,
    )
    .unwrap();
    let metadata = project_model(
        &ModelRef::parse("gemini/gemini-4-hydrogen").unwrap(),
        &model,
    );
    assert_eq!(metadata.context_window_tokens, Some(1_048_576));
    assert_eq!(metadata.default_max_output_tokens, Some(65_536));
    assert_eq!(metadata.max_output_tokens_upper_limit, Some(65_536));
    assert_eq!(
        metadata.reasoning_effort_options,
        ["low", "medium", "high", "xhigh"]
    );
    assert!(metadata.capabilities.image_input);
    assert!(metadata.capabilities.supports_reasoning);
    assert!(!metadata.capabilities.image_generation);
}

#[test]
fn moonshot_k3_default_output_is_not_the_capability_ceiling() {
    let model: ModelsDevModel = serde_json::from_str(
        r#"{"id":"kimi-k3.1-pro","limit":{"context":1048576,"output":1048576}}"#,
    )
    .unwrap();
    for model_ref in [
        "moonshot/kimi-k3",
        "moonshot/kimi-k3.1-pro",
        "moonshot/kimi-k3.1-pro-highspeed",
    ] {
        let metadata = project_model(&ModelRef::parse(model_ref).unwrap(), &model);
        assert_eq!(metadata.context_window_tokens, Some(1_048_576));
        assert_eq!(metadata.default_max_output_tokens, Some(131_072));
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(1_048_576));
    }
    for model_ref in ["opencode-go/kimi-k3.1-pro", "moonshot/k3.1-pro"] {
        let metadata = project_model(&ModelRef::parse(model_ref).unwrap(), &model);
        assert_eq!(metadata.default_max_output_tokens, Some(1_048_576));
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(1_048_576));
    }
}

#[test]
fn openrouter_mistral_large_4_efforts_preserve_endpoint_contract() {
    let model: ModelsDevModel = serde_json::from_str(
        r#"{
            "id":"mistralai/mistral-large-4-0",
            "reasoning":true,
            "reasoning_options":[{"type":"effort","values":["low","medium","high"]}],
            "limit":{"context":1048576,"output":262144}
        }"#,
    )
    .unwrap();
    let metadata = project_model(
        &ModelRef::parse("openrouter/mistralai/mistral-large-4-0").unwrap(),
        &model,
    );
    assert_eq!(metadata.reasoning_effort_options, vec!["none", "high"]);
    assert!(metadata.capabilities.supports_reasoning);
    assert_eq!(metadata.context_window_tokens, Some(1_048_576));
    assert_eq!(metadata.default_max_output_tokens, Some(262_144));
    assert_eq!(metadata.max_output_tokens_upper_limit, Some(262_144));

    for model_ref in [
        "kilo/mistralai/mistral-large-4-0",
        "mistral/mistral-large-4-0",
        "openrouter/mistralai/mistral-large-4-0-8",
    ] {
        let metadata = project_model(&ModelRef::parse(model_ref).unwrap(), &model);
        assert_eq!(
            metadata.reasoning_effort_options,
            vec!["low", "medium", "high"],
            "{model_ref}"
        );
    }
}

#[test]
fn projects_mapped_provider() {
    let snapshot = sample_snapshot();
    let result = Projector::new().project(&snapshot).unwrap();

    assert_eq!(result.projected.len(), 2);
    assert_eq!(result.unmapped.len(), 1);

    let gpt55 = result
        .projected
        .iter()
        .find(|m| m.models_dev_model_id == "gpt-5.5")
        .unwrap();
    assert_eq!(gpt55.metadata.model_ref.as_string(), "openai/gpt-5.5");
    assert_eq!(gpt55.metadata.display_name, "GPT-5.5");
    assert_eq!(gpt55.metadata.context_window_tokens, Some(1_050_000));
    assert_eq!(gpt55.metadata.default_max_output_tokens, Some(128_000));
    assert_eq!(gpt55.metadata.max_output_tokens_upper_limit, Some(128_000));
    assert!(gpt55.metadata.capabilities.supports_reasoning);
    assert!(gpt55.metadata.capabilities.image_input);
    assert!(!gpt55.metadata.capabilities.image_generation);
    assert_eq!(
        gpt55.metadata.reasoning_effort_options,
        vec!["none", "low", "high"]
    );
    assert_eq!(gpt55.metadata.source, ModelMetadataSource::RemoteDiscovered);
    assert!(gpt55.metadata.endpoint.is_none());
}

#[test]
fn projects_image_generation() {
    let snapshot = sample_snapshot();
    let result = Projector::new().project(&snapshot).unwrap();

    let gpt4o = result
        .projected
        .iter()
        .find(|m| m.models_dev_model_id == "gpt-4o-mini")
        .unwrap();
    assert!(gpt4o.metadata.capabilities.image_generation);
    assert!(!gpt4o.metadata.capabilities.supports_reasoning);
}

#[test]
fn skips_unmapped_provider() {
    let snapshot = sample_snapshot();
    let result = Projector::new().project(&snapshot).unwrap();

    assert_eq!(result.unmapped.len(), 1);
    assert_eq!(
        result.unmapped[0].models_dev_provider_id,
        "unknown-provider"
    );
    assert_eq!(result.unmapped[0].models_dev_model_id, "mystery-model");
}

#[test]
fn dated_seed_context_does_not_inherit_a_newer_versions_capacity() {
    for (route, context) in [
        ("volcengine/doubao-seed-2-1-pro-260628", None),
        ("volcengine/doubao-seed-2-1-pro-260915", Some(1_024_000)),
        ("openrouter/doubao-seed-2-1-pro-260628", Some(1_024_000)),
    ] {
        let model_ref = ModelRef::parse(route).unwrap();
        let model: ModelsDevModel = serde_json::from_value(serde_json::json!({
            "id": model_ref.model,
            "limit": {"context": 1_024_000, "output": 256_000}
        }))
        .unwrap();
        let metadata = project_model(&model_ref, &model);
        assert_eq!(metadata.context_window_tokens, context, "{route}");
        assert_eq!(
            metadata.auto_compact_token_limit,
            context.map(|value| value * DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT as usize / 100),
            "{route}"
        );
        assert_eq!(metadata.max_output_tokens_upper_limit, Some(256_000));
    }
}

#[test]
fn auto_compact_token_limit_derived() {
    let snapshot = sample_snapshot();
    let result = Projector::new().project(&snapshot).unwrap();

    let gpt55 = result
        .projected
        .iter()
        .find(|m| m.models_dev_model_id == "gpt-5.5")
        .unwrap();
    // 1050000 * 95 / 100 = 997500
    assert_eq!(gpt55.metadata.auto_compact_token_limit, Some(997_500));
}

#[test]
fn custom_mapping_overrides_default() {
    let snapshot = sample_snapshot();
    let projector = Projector::with_mappings(vec![ProviderMapping {
        models_dev_id: "openai".to_string(),
        holon_provider_id: "openrouter".to_string(),
    }]);
    let result = projector.project(&snapshot).unwrap();

    assert_eq!(result.projected.len(), 2);
    assert!(result.projected[0]
        .metadata
        .model_ref
        .provider
        .as_str()
        .contains("openrouter"));
}

#[test]
fn seed_character_limits_do_not_inherit_conflicting_upstream_capacity() {
    for (route, expected_context, expected_output) in [
        ("volcengine/doubao-seed-character-260628", None, None),
        (
            "volcengine/doubao-seed-character-251128",
            Some(256_000),
            Some(256_000),
        ),
        (
            "openrouter/doubao-seed-character-260628",
            Some(256_000),
            Some(256_000),
        ),
    ] {
        let model_ref = ModelRef::parse(route).unwrap();
        let model: ModelsDevModel = serde_json::from_value(serde_json::json!({
            "id": model_ref.model,
            "reasoning": true,
            "tool_call": true,
            "modalities": {"input": ["text", "image"], "output": ["text"]},
            "limit": {"context": 256_000, "output": 256_000}
        }))
        .unwrap();
        let metadata = project_model(&model_ref, &model);
        assert_eq!(metadata.context_window_tokens, expected_context, "{route}");
        assert_eq!(
            metadata.default_max_output_tokens, expected_output,
            "{route}"
        );
        assert_eq!(
            metadata.max_output_tokens_upper_limit, expected_output,
            "{route}"
        );
        assert_eq!(
            metadata.auto_compact_token_limit,
            expected_context
                .map(|value| value * DEFAULT_EFFECTIVE_CONTEXT_WINDOW_PERCENT as usize / 100),
            "{route}"
        );
        assert!(metadata.capabilities.agent_turn, "{route}");
        assert!(metadata.capabilities.image_input, "{route}");
        assert!(metadata.capabilities.supports_reasoning, "{route}");
    }
}
