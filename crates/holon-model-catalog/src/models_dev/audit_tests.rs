use super::*;

fn sample_snapshot() -> ModelsDevSnapshot {
    let json = r#"{
        "openai": { "id": "openai", "models": {} },
        "moonshotai": { "id": "moonshotai", "models": {} },
        "hpc-ai": { "id": "hpc-ai", "models": {} }
    }"#;
    serde_json::from_str(json).unwrap()
}

#[test]
fn audit_reports_mapped_unmapped_and_stale() {
    let snapshot = sample_snapshot();
    let mappings = vec![
        ProviderMapping {
            models_dev_id: "openai".into(),
            holon_provider_id: "openai".into(),
        },
        ProviderMapping {
            models_dev_id: "moonshotai".into(),
            holon_provider_id: "moonshot".into(),
        },
        ProviderMapping {
            models_dev_id: "nonexistent".into(),
            holon_provider_id: "ghost".into(),
        },
    ];
    let report = audit_mappings(&snapshot, &mappings);
    assert_eq!(report.mapped.len(), 2);
    assert!(report.mapped.iter().any(|m| m.models_dev_id == "openai"));
    assert!(report
        .mapped
        .iter()
        .any(|m| m.models_dev_id == "moonshotai" && m.holon_provider_id == "moonshot"));
    assert_eq!(report.stale_holon.len(), 1);
    assert_eq!(report.stale_holon[0].models_dev_id, "nonexistent");
    assert!(report.unmapped_upstream.contains(&"hpc-ai".to_string()));
}

#[test]
fn audit_default_mappings_have_no_stale() {
    let json = r#"{
        "openai":{"id":"openai","models":{}},"anthropic":{"id":"anthropic","models":{}},
        "deepseek":{"id":"deepseek","models":{}},"moonshotai":{"id":"moonshotai","models":{}},
        "togetherai":{"id":"togetherai","models":{}},"fireworks-ai":{"id":"fireworks-ai","models":{}},
        "google":{"id":"google","models":{}},"zhipuai":{"id":"zhipuai","models":{}},
        "alibaba":{"id":"alibaba","models":{}},"nvidia":{"id":"nvidia","models":{}},
        "openrouter":{"id":"openrouter","models":{}},"huggingface":{"id":"huggingface","models":{}},
        "xai":{"id":"xai","models":{}},"minimax":{"id":"minimax","models":{}},
        "zai":{"id":"zai","models":{}},"volcengine":{"id":"volcengine","models":{}},
        "stepfun":{"id":"stepfun","models":{}},"arcee":{"id":"arcee","models":{}},
        "chutes":{"id":"chutes","models":{}},"nearai":{"id":"nearai","models":{}},
        "venice":{"id":"venice","models":{}},"xiaomi":{"id":"xiaomi","models":{}},
        "mistral":{"id":"mistral","models":{}}
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    let report = audit_mappings(&snapshot, &ProviderMapping::default_mappings());
    assert!(
        report.stale_holon.is_empty(),
        "stale mappings: {:?}",
        report.stale_holon
    );
    assert!(!report.mapped.is_empty());
}
