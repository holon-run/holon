use super::*;
use crate::snapshot;

fn snapshot_json() -> String {
    r#"{
        "mistral": {
            "id": "mistral",
            "models": {
                "magistral-small": {"id": "magistral-small", "release_date": "2026-08-30",
                    "modalities": {"input": ["text"], "output": ["text"]}}
            }
        },
        "xai": {
            "id": "xai",
            "models": {
                "grok-4.3": {"id": "grok-4.3", "name": "Grok 4.3", "release_date": "2026-01-10",
                    "modalities": {"input": ["text"], "output": ["text"]}},
                "grok-new-9": {"id": "grok-new-9", "name": "Grok New 9", "release_date": "2026-08-30",
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "limit": {"context": 400000, "output": 64000}, "reasoning": true},
                "grok-old-1": {"id": "grok-old-1", "name": "Grok Old 1", "release_date": "2024-01-01",
                    "modalities": {"input": ["text"], "output": ["text"]}},
                "grok-nodate": {"id": "grok-nodate", "name": "Grok No Date",
                    "modalities": {"input": ["text"], "output": ["text"]}},
                "grok-image": {"id": "grok-image", "release_date": "2026-08-30",
                    "modalities": {"input": ["text"], "output": ["image"]}},
                "grok-retained": {"id": "grok-retained", "name": "Grok Retained", "release_date": "2026-01-05",
                    "modalities": {"input": ["text"], "output": ["text"]}}
            }
        },
        "openrouter": {
            "id": "openrouter",
            "models": {
                "some/model": {"id": "some/model", "release_date": "2026-08-30",
                    "modalities": {"input": ["text"], "output": ["text"]}}
            }
        }
    }"#
    .to_string()
}

fn legacy() -> BuiltInModelCatalog {
    snapshot::legacy_catalog().expect("legacy catalog must parse")
}

fn supplement_metadata(model_ref: &str, display_name: &str) -> BuiltInModelMetadata {
    BuiltInModelMetadata {
        model_ref: ModelRef::parse(model_ref).unwrap(),
        display_name: display_name.to_string(),
        description: "supplement test entry".to_string(),
        context_window_tokens: None,
        effective_context_window_percent: 95,
        auto_compact_token_limit: None,
        default_max_output_tokens: None,
        max_output_tokens_upper_limit: None,
        default_verbosity: None,
        tool_output_truncation_estimated_tokens: Some(2_500),
        capabilities: Default::default(),
        reasoning_effort_options: Vec::new(),
        source: ModelMetadataSource::ModelsDevSupplement,
        endpoint: None,
    }
}

fn previous() -> ModelsDevSupplement {
    let metadata = supplement_metadata("xai/grok-retained", "Grok Retained");
    let vanished = supplement_metadata("xai/grok-vanished", "Grok Vanished");
    ModelsDevSupplement {
        schema_version: SUPPLEMENT_SCHEMA_VERSION,
        upstream_revision: "abc".into(),
        adapter_version: "test".into(),
        models: vec![metadata, vanished],
        routes: Vec::new(),
    }
}

#[test]
fn drafts_only_recent_text_models_of_allowlisted_providers() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
    let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();

    let drafted_refs: Vec<String> = update
        .drafted
        .iter()
        .map(|model| model.model_ref.as_string())
        .collect();
    assert_eq!(drafted_refs, vec!["xai/grok-new-9".to_string()]);
    let drafted_model = &update.drafted[0];
    assert_eq!(
        drafted_model.source,
        ModelMetadataSource::ModelsDevSupplement
    );
    assert_eq!(drafted_model.context_window_tokens, Some(400_000));
    assert!(drafted_model.capabilities.supports_reasoning);
    assert!(drafted_model.capabilities.image_input);

    let deferred: Vec<(&str, DeferredReason)> = update
        .deferred
        .iter()
        .map(|d| (d.model_ref.as_str(), d.reason))
        .collect();
    assert!(deferred.contains(&("xai/grok-old-1", DeferredReason::ReleaseOutsideWindow)));
    assert!(deferred.contains(&("xai/grok-nodate", DeferredReason::NoReleaseDate)));
    assert!(deferred.contains(&("xai/grok-image", DeferredReason::NotTextOutput)));
    assert_eq!(
        update.providers_not_allowlisted,
        vec!["openrouter".to_string()]
    );
}

#[test]
fn drafts_api_models_but_defers_unreviewed_codex_aliases() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(
        r#"{
            "openai": {
                "id": "openai",
                "models": {
                    "gpt-future-test": {
                        "id": "gpt-future-test",
                        "name": "GPT Future Test",
                        "release_date": "2026-08-30",
                        "modalities": {
                            "input": ["text", "image"],
                            "output": ["text"]
                        }
                    }
                }
            }
        }"#,
    )
    .unwrap();

    let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();
    let drafted_refs: Vec<String> = update
        .drafted
        .iter()
        .map(|model| model.model_ref.as_string())
        .collect();

    assert_eq!(drafted_refs, vec!["openai/gpt-future-test".to_string()]);
    assert!(update.deferred.iter().any(|model| {
        model.model_ref == "openai-codex/gpt-future-test"
            && model.reason == DeferredReason::EndpointReviewRequired
    }));
}

#[test]
fn defers_unreviewed_mistral_metadata_without_excluding_reviewed_peers() {
    let parsed: ModelsDevSnapshot = serde_json::from_value(serde_json::json!({
        "mistral": {
            "id": "mistral",
            "models": {
                "deepseek-v4-1-flash-1017": {
                    "id": "deepseek-v4-1-flash-1017",
                    "name": "DeepSeek V4.1 Flash",
                    "release_date": "2026-10-28",
                    "modalities": { "input": ["text", "image"], "output": ["text"] }
                },
                "mistral-large-4-0-8": {
                    "id": "mistral-large-4-0-8",
                    "name": "Mistral Large 4.0.8",
                    "release_date": "2026-10-30",
                    "limit": { "context": 1048576, "output": 262144 },
                    "modalities": { "input": ["text", "image"], "output": ["text"] }
                },
                "reviewed-peer": {
                    "id": "reviewed-peer",
                    "name": "Reviewed Peer",
                    "release_date": "2026-10-28",
                    "modalities": { "input": ["text"], "output": ["text"] }
                }
            }
        }
    }))
    .unwrap();
    let previous = ModelsDevSupplement {
        schema_version: SUPPLEMENT_SCHEMA_VERSION,
        upstream_revision: "previous".into(),
        adapter_version: "test".into(),
        models: parsed.providers["mistral"]
            .models
            .iter()
            .map(|(id, model)| {
                project_model(&ModelRef::parse(&format!("mistral/{id}")).unwrap(), model)
            })
            .collect(),
        routes: Vec::new(),
    };
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy(), "2026-05-05", "rev", "ver").unwrap();
        assert_eq!(
            update
                .supplement
                .models
                .iter()
                .map(|model| model.model_ref.as_string())
                .collect::<Vec<_>>(),
            ["mistral/reviewed-peer"]
        );
        for id in ["deepseek-v4-1-flash-1017", "mistral-large-4-0-8"] {
            assert!(update.deferred.iter().any(|model| {
                model.model_ref == format!("mistral/{id}")
                    && model.reason == DeferredReason::EndpointReviewRequired
            }));
        }
    }
}

#[test]
fn retains_openai_codex_models_from_the_openai_snapshot_provider() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(
        r#"{
            "openai": {
                "id": "openai",
                "models": {
                    "gpt-future-test": {
                        "id": "gpt-future-test",
                        "name": "GPT Future Test",
                        "release_date": "2026-08-30",
                        "modalities": {
                            "input": ["text", "image"],
                            "output": ["text"]
                        }
                    }
                }
            }
        }"#,
    )
    .unwrap();
    let previous = ModelsDevSupplement {
        schema_version: SUPPLEMENT_SCHEMA_VERSION,
        upstream_revision: "abc".into(),
        adapter_version: "test".into(),
        models: vec![supplement_metadata(
            "openai-codex/gpt-future-test",
            "GPT Future Test",
        )],
        routes: Vec::new(),
    };

    let update = generate(
        &parsed,
        Some(&previous),
        &legacy(),
        "2026-05-05",
        "rev",
        "ver",
    )
    .unwrap();

    assert_eq!(
        update.retained,
        vec!["openai-codex/gpt-future-test".to_string()]
    );
    assert_eq!(
        update
            .drafted
            .iter()
            .map(|model| model.model_ref.as_string())
            .collect::<Vec<_>>(),
        vec!["openai/gpt-future-test".to_string()]
    );
    let retained = update
        .supplement
        .models
        .iter()
        .find(|model| model.model_ref.as_string() == "openai-codex/gpt-future-test")
        .expect("retained codex model");
    assert_eq!(retained.display_name, "GPT Future Test");
    assert_eq!(retained.context_window_tokens, None);
}

#[test]
fn excludes_retired_codex_models_without_removing_api_models() {
    let legacy = legacy();
    for model_id in [
        "gpt-5.2",
        "gpt-5.3-codex",
        "gpt-5.3-codex-spark",
        "gpt-5.4",
        "gpt-5.4-mini",
        "gpt-5.5",
    ] {
        let mut parsed: ModelsDevSnapshot =
            serde_json::from_str(r#"{"openai":{"id":"openai","models":{}}}"#).unwrap();
        parsed.providers.get_mut("openai").unwrap().models.insert(
            model_id.to_string(),
            serde_json::from_value(serde_json::json!({
                "id": model_id,
                "release_date": "2026-10-14",
                "modalities": {"input": ["text", "image"], "output": ["text"]}
            }))
            .unwrap(),
        );
        let codex_ref = format!("openai-codex/{model_id}");
        let previous = ModelsDevSupplement {
            schema_version: SUPPLEMENT_SCHEMA_VERSION,
            upstream_revision: "abc".into(),
            adapter_version: "test".into(),
            models: vec![supplement_metadata(&codex_ref, model_id)],
            routes: Vec::new(),
        };
        for previous in [None, Some(&previous)] {
            let update = generate(&parsed, previous, &legacy, "2026-10-01", "rev", "ver").unwrap();
            assert!(!update
                .supplement
                .models
                .iter()
                .any(|model| { model.model_ref.as_string() == codex_ref }));
            let api_ref = ModelRef::parse(&format!("openai/{model_id}")).unwrap();
            assert!(
                legacy.entries.contains_key(&api_ref)
                    || update
                        .supplement
                        .models
                        .iter()
                        .any(|model| model.model_ref == api_ref),
                "{} must remain available in the base catalog or supplement",
                api_ref.as_string()
            );
            if previous.is_some() {
                assert!(update.removed.iter().any(|removed| {
                    removed.model_ref == codex_ref
                        && removed.reason == RemovalReason::UnsupportedByProvider
                }));
            } else {
                assert!(update.deferred.iter().any(|deferred| {
                    deferred.model_ref == codex_ref
                        && deferred.reason == DeferredReason::UnsupportedByProvider
                }));
            }
        }
    }
}

#[test]
fn excludes_retired_xiaomi_chat_ids_from_new_and_retained_supplements() {
    let legacy = legacy();
    let mut parsed: ModelsDevSnapshot =
        serde_json::from_str(r#"{"xiaomi":{"id":"xiaomi","models":{}}}"#).unwrap();
    for id in ["mimo-v2.5-pro", "mimo-v2.5", "mimo-v2.5-pro-ultraspeed"] {
        let model = serde_json::from_value(serde_json::json!({
            "id": id,
            "release_date": "2026-10-21",
            "modalities": {"input": ["text"], "output": ["text"]}
        }))
        .unwrap();
        for provider in ["tencent-tokenhub", "openrouter"] {
            assert!(supplement_model_supported_by_provider(provider, id, &model));
        }
        parsed
            .providers
            .get_mut("xiaomi")
            .unwrap()
            .models
            .insert(id.into(), model);
    }
    let retired = ["xiaomi/mimo-v2.5-pro", "xiaomi/mimo-v2.5"];
    let mut previous = ModelsDevSupplement::empty();
    previous.models = retired
        .iter()
        .map(|id| supplement_metadata(id, id))
        .collect();
    previous.routes = retired
        .iter()
        .map(|id| ModelRouteRef::parse(&id.replace("xiaomi/", "xiaomi@token-plan/")).unwrap())
        .collect();
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy, "2026-10-01", "rev", "ver").unwrap();
        for id in retired {
            assert!(!update
                .supplement
                .models
                .iter()
                .any(|m| m.model_ref.as_string() == id));
            if previous.is_some() {
                assert!(update.removed.iter().any(|removed| {
                    removed.model_ref == id
                        && removed.reason == RemovalReason::UnsupportedByProvider
                }));
            } else {
                assert!(update.deferred.iter().any(|deferred| {
                    deferred.model_ref == id
                        && deferred.reason == DeferredReason::UnsupportedByProvider
                }));
            }
        }
        assert!(update.supplement.routes.is_empty());
        assert!(update
            .supplement
            .models
            .iter()
            .any(|model| { model.model_ref.as_string() == "xiaomi/mimo-v2.5-pro-ultraspeed" }));
    }
}

#[test]
fn retired_moonshot_ids_are_excluded_from_new_and_retained_supplements() {
    let legacy = legacy();
    let ids = [
        "kimi-k2.5",
        "moonshot-v1-8k",
        "moonshot-v1-32k",
        "moonshot-v1-128k",
        "moonshot-v1-auto",
        "moonshot-v1-8k-vision-preview",
        "moonshot-v1-32k-vision-preview",
        "moonshot-v1-128k-vision-preview",
    ];
    let mut parsed: ModelsDevSnapshot =
        serde_json::from_str(r#"{"moonshotai":{"id":"moonshotai","models":{}}}"#).unwrap();
    let mut previous = ModelsDevSupplement::empty();
    for id in ids {
        let model = serde_json::from_value(serde_json::json!({
            "id": id, "release_date": "2026-10-24",
            "modalities": {"input": ["text"], "output": ["text"]}
        }))
        .unwrap();
        for provider in ["tencent-tokenhub", "opencode-go", "openrouter"] {
            assert!(supplement_model_supported_by_provider(provider, id, &model));
        }
        parsed
            .providers
            .get_mut("moonshotai")
            .unwrap()
            .models
            .insert(id.into(), model);
        previous
            .models
            .push(supplement_metadata(&format!("moonshot/{id}"), id));
        previous
            .routes
            .push(ModelRouteRef::parse(&format!("moonshot@default/{id}")).unwrap());
    }
    let model = parsed.providers["moonshotai"]
        .models
        .values()
        .next()
        .unwrap();
    for id in ["kimi-k2.6", "kimi-k2.7-code", "kimi-k3", "kimi-k3.1-pro"] {
        assert!(supplement_model_supported_by_provider(
            "moonshot", id, model
        ));
    }
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy, "2026-10-25", "rev", "ver").unwrap();
        assert!(update.supplement.models.is_empty());
        assert!(update.supplement.routes.is_empty());
        for id in ids {
            let model_ref = format!("moonshot/{id}");
            if previous.is_some() {
                assert!(update.removed.iter().any(|removed| {
                    removed.model_ref == model_ref
                        && removed.reason == RemovalReason::UnsupportedByProvider
                }));
            } else {
                assert!(update.deferred.iter().any(|deferred| {
                    deferred.model_ref == model_ref
                        && deferred.reason == DeferredReason::UnsupportedByProvider
                }));
            }
        }
    }
}

#[test]
fn excludes_retired_minimax_api_ids_without_retiring_peers_or_future_ids() {
    let legacy = legacy();
    let ids = ["MiniMax-M2", "MiniMax-M2.1", "MiniMax-M2.1-highspeed"];
    let mut parsed: ModelsDevSnapshot =
        serde_json::from_str(r#"{"minimax":{"id":"minimax","models":{}}}"#).unwrap();
    let mut previous = ModelsDevSupplement::empty();
    for id in ids {
        let model = serde_json::from_value(serde_json::json!({
            "id": id, "release_date": "2026-10-01",
            "modalities": {"input": ["text"], "output": ["text"]}
        }))
        .unwrap();
        for provider in ["minimax-cn", "tencent-tokenhub", "openrouter"] {
            assert!(supplement_model_supported_by_provider(provider, id, &model));
        }
        parsed
            .providers
            .get_mut("minimax")
            .unwrap()
            .models
            .insert(id.into(), model);
        previous
            .models
            .push(supplement_metadata(&format!("minimax/{id}"), id));
    }
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy, "2026-10-23", "rev", "ver").unwrap();
        assert!(update.supplement.models.is_empty());
        for id in ids {
            let model_ref = format!("minimax/{id}");
            if previous.is_some() {
                assert!(update.removed.iter().any(|removed| {
                    removed.model_ref == model_ref
                        && removed.reason == RemovalReason::UnsupportedByProvider
                }));
            } else {
                assert!(update.deferred.iter().any(|deferred| {
                    deferred.model_ref == model_ref
                        && deferred.reason == DeferredReason::UnsupportedByProvider
                }));
            }
        }
    }
    let model = parsed.providers["minimax"].models.values().next().unwrap();
    for id in ["MiniMax-M2.5", "MiniMax-M2.7", "MiniMax-M3"] {
        assert!(supplement_model_supported_by_provider("minimax", id, model));
    }
}

#[test]
fn tokenhub_retired_models_are_not_restored_from_previous_supplements() {
    let ids = [
        "deepseek-v3.2",
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "glm-5",
        "glm-5-turbo",
        "glm-5.1",
        "glm-5v-turbo",
        "hy3-preview",
        "kimi-k2.5",
        "mimo-v2.5-pro",
        "minimax-m2.5",
        "minimax-m2.7",
        "minimax-m-2-7",
        "qwen3.5-flash",
        "qwen3.5-plus",
        "youtu-vita",
    ];
    let responses = [
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "mimo-v2.5-pro",
        "minimax-m2.7",
        "minimax-m-2-7",
        "qwen3.5-flash",
        "qwen3.5-plus",
    ];
    let mut parsed: ModelsDevSnapshot = serde_json::from_value(serde_json::json!({
        "tencent-tokenhub": {"id": "tencent-tokenhub", "models": {}}
    }))
    .unwrap();
    let mut previous = ModelsDevSupplement::empty();
    for id in ids {
        parsed
            .providers
            .get_mut("tencent-tokenhub")
            .unwrap()
            .models
            .insert(
                id.into(),
                serde_json::from_value(serde_json::json!({
                    "id": id, "release_date": "2026-11-01",
                    "modalities": {"input": ["text"], "output": ["text"]}
                }))
                .unwrap(),
            );
        previous
            .models
            .push(supplement_metadata(&format!("tencent-tokenhub/{id}"), id));
        for endpoint in ["default", "messages", "responses"] {
            if endpoint == "responses" && !responses.contains(&id) {
                continue;
            }
            previous
                .routes
                .push(ModelRouteRef::parse(&format!("tencent-tokenhub@{endpoint}/{id}")).unwrap());
        }
    }
    assert_eq!(previous.routes.len(), 39);
    // Gateway catalogs cannot auto-admit models, including retained entries.
    assert!(!AUTO_SUPPLEMENT_PROVIDERS.contains(&"tencent-tokenhub"));
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy(), "2026-11-04", "rev", "ver").unwrap();
        assert!(update.supplement.models.is_empty());
        assert!(update.supplement.routes.is_empty());
        if previous.is_some() {
            assert_eq!(update.removed.len(), ids.len());
        }
    }
}

#[test]
fn retired_qianfan_ids_are_not_drafted_or_retained() {
    // Qianfan has no upstream mapping or supplement admission. Do not
    // broaden either to filter IDs already excluded by that boundary.
    let ids = [
        "ernie-5.0",
        "ernie-5.0-thinking-latest",
        "ernie-5.0-thinking-preview",
        "ernie-x1.1",
        "deepseek-v3.2",
        "deepseek-v3.2-think",
    ];
    let mut parsed: ModelsDevSnapshot =
        serde_json::from_str(r#"{"qianfan":{"id":"qianfan","models":{}}}"#).unwrap();
    let mut previous = ModelsDevSupplement::empty();
    for id in ids {
        parsed.providers.get_mut("qianfan").unwrap().models.insert(
            id.into(),
            serde_json::from_value(serde_json::json!({
                "id": id, "release_date": "2026-10-29",
                "modalities": {"input": ["text"], "output": ["text"]}
            }))
            .unwrap(),
        );
        previous
            .models
            .push(supplement_metadata(&format!("qianfan/{id}"), id));
        previous
            .routes
            .push(ModelRouteRef::parse(&format!("qianfan@default/{id}")).unwrap());
    }
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy(), "2026-10-29", "rev", "ver").unwrap();
        assert!(update.supplement.models.is_empty());
        assert!(update.supplement.routes.is_empty());
        if previous.is_some() {
            for id in ids {
                assert!(update.removed.iter().any(|removed| {
                    removed.model_ref == format!("qianfan/{id}")
                        && removed.reason == RemovalReason::MissingUpstream
                }));
            }
        }
    }
}

#[test]
fn retired_volcengine_api_ids_are_not_drafted_or_retained() {
    let legacy = legacy();
    let ids = [
        "deepseek-v3-2-251201",
        "doubao-seed-1-8-251228",
        "doubao-seed-1-6-251015",
        "doubao-seed-1-6-flash-250828",
        "doubao-seed-1-6-vision-250815",
        "doubao-seed-2-0-lite-260215",
        "doubao-seed-2-0-mini-260215",
    ];
    let mut parsed: ModelsDevSnapshot =
        serde_json::from_str(r#"{"volcengine":{"id":"volcengine","models":{}}}"#).unwrap();
    let mut previous = ModelsDevSupplement::empty();
    for id in ids {
        let model = serde_json::from_value(serde_json::json!({
            "id": id, "release_date": "2026-10-26",
            "modalities": {"input": ["text"], "output": ["text"]}
        }))
        .unwrap();
        for provider in ["volcengine-coding", "volcengine-agent", "openrouter"] {
            assert!(supplement_model_supported_by_provider(provider, id, &model));
        }
        parsed
            .providers
            .get_mut("volcengine")
            .unwrap()
            .models
            .insert(id.into(), model);
        previous
            .models
            .push(supplement_metadata(&format!("volcengine/{id}"), id));
    }
    let sample = parsed.providers["volcengine"]
        .models
        .values()
        .next()
        .unwrap();
    for id in [
        "doubao-seed-2-0-pro-260215",
        "doubao-seed-2-0-code-preview-260215",
        "kimi-k2.7-code",
        "minimax-m3",
    ] {
        assert!(supplement_model_supported_by_provider(
            "volcengine",
            id,
            sample
        ));
    }
    for previous in [None, Some(&previous)] {
        let update = generate(&parsed, previous, &legacy, "2026-10-26", "rev", "ver").unwrap();
        assert!(update.supplement.models.is_empty());
        assert!(update.supplement.routes.is_empty());
        for id in ids {
            let model_ref = format!("volcengine/{id}");
            if previous.is_some() {
                assert!(update.removed.iter().any(|removed| {
                    removed.model_ref == model_ref
                        && removed.reason == RemovalReason::UnsupportedByProvider
                }));
            } else {
                assert!(update.deferred.iter().any(|deferred| {
                    deferred.model_ref == model_ref
                        && deferred.reason == DeferredReason::UnsupportedByProvider
                }));
            }
        }
    }
}

#[test]
fn retired_volcengine_subscription_routes_do_not_retire_the_api_model() {
    let mut legacy = legacy();
    // Exercise retained routes independently of legacy model promotion.
    for model in ["kimi-k2.7-code", "minimax-m3"] {
        legacy
            .entries
            .remove(&ModelRef::parse(&format!("volcengine/{model}")).unwrap());
    }
    let parsed: ModelsDevSnapshot = serde_json::from_str(
        r#"{
        "volcengine":{"id":"volcengine","models":{
            "kimi-k2.6":{"id":"kimi-k2.6","release_date":"2026-10-01",
                "modalities":{"input":["text"],"output":["text"]}},
            "kimi-k2.7-code":{"id":"kimi-k2.7-code","release_date":"2026-10-01",
                "modalities":{"input":["text"],"output":["text"]}},
            "minimax-m3":{"id":"minimax-m3","release_date":"2026-10-01",
                "modalities":{"input":["text"],"output":["text"]}}
        }}
    }"#,
    )
    .unwrap();
    let mut previous = ModelsDevSupplement::empty();
    previous
        .models
        .push(supplement_metadata("volcengine/kimi-k2.6", "Kimi K2.6"));
    previous.models.push(supplement_metadata(
        "volcengine/kimi-k2.7-code",
        "Kimi K2.7 Code",
    ));
    previous
        .models
        .push(supplement_metadata("volcengine/minimax-m3", "MiniMax M3"));
    previous.routes = ["kimi-k2.6", "kimi-k2.7-code", "minimax-m3"]
        .into_iter()
        .flat_map(|model| {
            ["default", "coding", "plan"].map(|endpoint| {
                ModelRouteRef::parse(&format!("volcengine@{endpoint}/{model}")).unwrap()
            })
        })
        .collect();
    let update = generate(
        &parsed,
        Some(&previous),
        &legacy,
        "2026-11-03",
        "rev",
        "ver",
    )
    .unwrap();
    assert_eq!(update.supplement.models.len(), 3);
    assert_eq!(
        update.supplement.routes,
        [
            ModelRouteRef::parse("volcengine@default/kimi-k2.6").unwrap(),
            ModelRouteRef::parse("volcengine@default/kimi-k2.7-code").unwrap(),
            ModelRouteRef::parse("volcengine@default/minimax-m3").unwrap(),
        ]
    );
}

#[test]
fn gemini_live_translation_is_not_drafted_or_retained_as_a_generate_content_route() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(
        r#"{
            "google": {
                "id": "google",
                "models": {
                    "gemini-3.5-live-translate-preview": {
                        "id": "gemini-3.5-live-translate-preview",
                        "name": "Live translation",
                        "release_date": "2026-11-16",
                        "limit": {"context": 16384, "output": 16384},
                        "modalities": {"input": ["audio"], "output": ["audio", "text"]}
                    }
                }
            }
        }"#,
    )
    .unwrap();
    for retained in [false, true] {
        let mut previous = ModelsDevSupplement::empty();
        if retained {
            previous.models.push(supplement_metadata(
                "gemini/gemini-3.5-live-translate-preview",
                "Live translation",
            ));
            previous.routes.push(
                ModelRouteRef::parse("gemini@default/gemini-3.5-live-translate-preview").unwrap(),
            );
        }
        let update = generate(
            &parsed,
            Some(&previous),
            &legacy(),
            "2026-11-17",
            "rev",
            "ver",
        )
        .unwrap();
        assert!(update.supplement.models.is_empty());
        assert!(update.supplement.routes.is_empty());
        if retained {
            assert_eq!(
                update.removed[0].reason,
                RemovalReason::UnsupportedByProvider
            );
        } else {
            assert_eq!(
                update.deferred[0].reason,
                DeferredReason::UnsupportedByProvider
            );
        }
    }
    let live = &parsed.providers["google"].models["gemini-3.5-live-translate-preview"];
    assert!(supplement_model_supported_by_provider(
        "openrouter",
        "gemini-3.5-live-translate-preview",
        live
    ));
}

#[test]
fn excludes_audio_models_from_the_openai_codex_projection() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(
        r#"{
            "openai": {
                "id": "openai",
                "models": {
                    "gpt-realtime-2.1": {
                        "id": "gpt-realtime-2.1",
                        "name": "GPT-Realtime-2.1",
                        "release_date": "2026-08-30",
                        "modalities": {
                            "input": ["text", "audio"],
                            "output": ["text", "audio"]
                        }
                    },
                    "gpt-realtime-2.2": {
                        "id": "gpt-realtime-2.2",
                        "name": "GPT-Realtime-2.2",
                        "release_date": "2026-08-31",
                        "modalities": {
                            "input": ["text", "audio"],
                            "output": ["text", "audio"]
                        }
                    }
                }
            }
        }"#,
    )
    .unwrap();
    let previous = ModelsDevSupplement {
        schema_version: SUPPLEMENT_SCHEMA_VERSION,
        upstream_revision: "abc".into(),
        adapter_version: "test".into(),
        models: vec![supplement_metadata(
            "openai-codex/gpt-realtime-2.1",
            "GPT-Realtime-2.1",
        )],
        routes: Vec::new(),
    };

    let update = generate(
        &parsed,
        Some(&previous),
        &legacy(),
        "2026-05-05",
        "rev",
        "ver",
    )
    .unwrap();

    assert!(!update
        .supplement
        .models
        .iter()
        .any(|model| { model.model_ref.as_string() == "openai-codex/gpt-realtime-2.1" }));
    assert!(update.removed.iter().any(|removed| {
        removed.model_ref == "openai-codex/gpt-realtime-2.1"
            && removed.reason == RemovalReason::UnsupportedByProvider
    }));
    assert!(update
        .supplement
        .models
        .iter()
        .any(|model| { model.model_ref.as_string() == "openai/gpt-realtime-2.1" }));
    assert!(update.deferred.iter().any(|deferred| {
        deferred.model_ref == "openai-codex/gpt-realtime-2.2"
            && deferred.reason == DeferredReason::UnsupportedByProvider
    }));
}

#[test]
fn subscription_routes_require_explicit_review_and_retain_only_current_models() {
    for (source, provider, endpoints) in [
        ("alibaba", "dashscope", &["coding-plan", "token-plan"][..]),
        ("stepfun", "stepfun", &["plan"][..]),
        ("volcengine", "volcengine", &["plan"][..]),
        ("xiaomi", "xiaomi", &["token-plan"][..]),
    ] {
        let parsed: ModelsDevSnapshot = serde_json::from_value(serde_json::json!({
            (source): {
                "id": source,
                "models": {
                    "reviewed-model": {
                        "id": "reviewed-model",
                        "name": "Reviewed model",
                        "release_date": "2026-01-01",
                        "modalities": { "input": ["text"], "output": ["text"] }
                    },
                    "new-api-model": {
                        "id": "new-api-model",
                        "name": "New API model",
                        "release_date": "2026-08-30",
                        "modalities": { "input": ["text"], "output": ["text"] }
                    }
                }
            }
        }))
        .unwrap();
        let reviewed_routes: Vec<ModelRouteRef> = endpoints
            .iter()
            .map(|endpoint| {
                ModelRouteRef::parse(&format!("{provider}@{endpoint}/reviewed-model")).unwrap()
            })
            .collect();
        let mut previous = ModelsDevSupplement {
            schema_version: SUPPLEMENT_SCHEMA_VERSION,
            upstream_revision: "abc".into(),
            adapter_version: "test".into(),
            models: vec![
                supplement_metadata(&format!("{provider}/reviewed-model"), "Reviewed model"),
                supplement_metadata(&format!("{provider}/removed-model"), "Removed model"),
            ],
            routes: reviewed_routes.clone(),
        };
        previous.routes.push(
            ModelRouteRef::parse(&format!("{provider}@{}/removed-model", endpoints[0])).unwrap(),
        );

        let update = generate(
            &parsed,
            Some(&previous),
            &legacy(),
            "2026-05-05",
            "rev",
            "ver",
        )
        .unwrap();
        assert_eq!(update.drafted.len(), 1, "{provider}");
        assert_eq!(
            update.drafted[0].model_ref.as_string(),
            format!("{provider}/new-api-model")
        );
        assert_eq!(update.supplement.routes, reviewed_routes, "{provider}");

        let fresh = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();
        assert!(fresh.supplement.routes.is_empty(), "{provider}");
    }
}

#[test]
fn retains_sticky_entries_and_removes_missing_ones() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
    let prev = previous();
    let update = generate(&parsed, Some(&prev), &legacy(), "2026-05-05", "rev", "ver").unwrap();

    assert_eq!(update.retained, vec!["xai/grok-retained".to_string()]);
    assert_eq!(update.drafted.len(), 1);
    let refs: Vec<String> = update
        .supplement
        .models
        .iter()
        .map(|model| model.model_ref.as_string())
        .collect();
    assert_eq!(
        refs,
        vec![
            "xai/grok-new-9".to_string(),
            "xai/grok-retained".to_string()
        ]
    );
    assert_eq!(
        update.removed,
        vec![RemovedModel {
            model_ref: "xai/grok-vanished".into(),
            reason: RemovalReason::MissingUpstream,
        }]
    );
}

#[test]
fn promotes_to_legacy_when_model_enters_builtin_catalog() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
    let mut prev = previous();
    let metadata = supplement_metadata("xai/grok-4.3", "Grok 4.3");
    prev.models.push(metadata);
    prev.routes
        .push(ModelRouteRef::parse("xai@plan/grok-4.3").unwrap());
    let update = generate(&parsed, Some(&prev), &legacy(), "2026-05-05", "rev", "ver").unwrap();
    assert!(update.removed.iter().any(|removed| {
        removed.model_ref == "xai/grok-4.3" && removed.reason == RemovalReason::PromotedToLegacy
    }));
    assert!(!update
        .supplement
        .models
        .iter()
        .any(|model| model.model_ref.as_string() == "xai/grok-4.3"));
    assert!(!update
        .supplement
        .routes
        .iter()
        .any(|route| { route.model_ref().as_string() == "xai/grok-4.3" }));
    update.supplement.validate().unwrap();
}

#[test]
fn defers_models_that_shadow_legacy_aliases() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
    let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();
    // mistral/magistral-small is a legacy alias; the snapshot lists it
    // under xai here as an unmapped id, so only the alias path matters.
    assert!(update
        .deferred
        .iter()
        .any(|deferred| { deferred.reason == DeferredReason::LegacyAliasConflict }));
}

#[test]
fn supplement_round_trips_through_json() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
    let update = generate(
        &parsed,
        Some(&previous()),
        &legacy(),
        "2026-05-05",
        "rev",
        "ver",
    )
    .unwrap();
    let json = update.supplement.to_json().unwrap();
    let parsed_back = ModelsDevSupplement::parse(&json).unwrap();
    assert_eq!(parsed_back, update.supplement);
}

#[test]
fn apply_to_rejects_collisions_and_inserts_routes() {
    let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
    let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();
    let mut catalog = legacy();
    update.supplement.apply_to(&mut catalog).unwrap();
    let model_ref = ModelRef::parse("xai/grok-new-9").unwrap();
    assert!(catalog.get(&model_ref).is_some());
    let route = catalog.get_route(&ModelRouteRef::parse("xai@default/grok-new-9").unwrap());
    assert!(route.is_some());

    // Collision: force the supplement to claim a legacy model.
    let mut colliding = update.supplement.clone();
    let metadata = supplement_metadata("xai/grok-4.3", "Grok 4.3");
    colliding.models.push(metadata);
    let error = colliding.apply_to(&mut legacy()).unwrap_err();
    assert!(error.contains("collides with the built-in catalog"));
}

#[test]
fn empty_bootstrap_parses_and_applies() {
    let json = serde_json::to_string_pretty(&ModelsDevSupplement::empty()).unwrap();
    let parsed = ModelsDevSupplement::parse(&json).unwrap();
    let mut catalog = legacy();
    parsed.apply_to(&mut catalog).unwrap();
    assert_eq!(catalog.list().len(), legacy().list().len());
}
