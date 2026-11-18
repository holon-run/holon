use super::*;

#[test]
fn parses_anthropic_manifest() {
    let raw = include_str!("../manifests/anthropic_v1.json");
    let manifest = ProviderMappingManifest::parse(raw).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.providers.len(), 1);
    let entry = &manifest.providers[0];
    assert_eq!(entry.models_dev_id, "anthropic");
    assert_eq!(entry.holon_provider_id, "anthropic");
    assert_eq!(entry.kind, ProviderKind::Direct);
    assert_eq!(entry.transport, "anthropic_messages");
    assert_eq!(entry.route_registration, "anthropic@default");
    assert!(!entry.enabled);
    assert_eq!(entry.provenance.owner, "holon");
    assert_eq!(entry.provenance.reviewed_at.as_deref(), Some("2026-08-31"));
}

#[test]
fn parses_deepseek_manifest() {
    let raw = include_str!("../manifests/deepseek_v1.json");
    let manifest = ProviderMappingManifest::parse(raw).unwrap();
    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.providers.len(), 1);
    let entry = &manifest.providers[0];
    assert_eq!(entry.models_dev_id, "deepseek");
    assert_eq!(entry.holon_provider_id, "deepseek");
    assert_eq!(entry.kind, ProviderKind::OpenAiCompatible);
    assert_eq!(entry.transport, "openai_responses");
    assert_eq!(entry.route_registration, "deepseek@responses");
    assert!(!entry.enabled);
    assert_eq!(entry.provenance.owner, "holon");
    assert_eq!(entry.provenance.reviewed_at.as_deref(), Some("2026-10-17"));
    assert_eq!(entry.model_id.mode, ModelIdMatchMode::Exact);
    assert_eq!(entry.model_id.allow.len(), 3);
    for model in ["deepseek-v4-pro", "deepseek-v4-flash", "deepseek-flash"] {
        assert!(is_model_allowed(&entry.model_id, model));
    }
    for model in [
        "deepseek-chat",
        "deepseek-reasoner",
        "deepseek-flash-preview",
        "deepseek-v4.1-flash",
    ] {
        assert!(!is_model_allowed(&entry.model_id, model));
    }
    // A provider ceiling permits vision; only model metadata can assert it.
    assert!(entry.capability_ceiling.image_input);
}

#[test]
fn exact_mode_only_matches_exact_ids() {
    let allow = ModelIdAllow {
        mode: ModelIdMatchMode::Exact,
        allow: vec!["claude-sonnet-4-20250514".to_string()],
    };
    assert!(is_model_allowed(&allow, "claude-sonnet-4-20250514"));
    assert!(!is_model_allowed(&allow, "claude-opus-4"));
    assert!(!is_model_allowed(&allow, "claude-sonnet-4-20250514-v2"));
}

#[test]
fn exact_or_pattern_matches_globs() {
    let allow = ModelIdAllow {
        mode: ModelIdMatchMode::ExactOrPattern,
        allow: vec!["claude-*".to_string()],
    };
    assert!(is_model_allowed(&allow, "claude-sonnet-4-20250514"));
    assert!(is_model_allowed(&allow, "claude-opus-4"));
    assert!(!is_model_allowed(&allow, "gpt-4o"));
}

#[test]
fn exact_or_pattern_matches_exact_id() {
    let allow = ModelIdAllow {
        mode: ModelIdMatchMode::ExactOrPattern,
        allow: vec!["claude-sonnet-4-20250514".to_string()],
    };
    assert!(is_model_allowed(&allow, "claude-sonnet-4-20250514"));
    assert!(!is_model_allowed(&allow, "claude-opus-4"));
}

#[test]
fn pattern_mode_requires_glob() {
    let allow = ModelIdAllow {
        mode: ModelIdMatchMode::Pattern,
        allow: vec!["claude-*".to_string()],
    };
    assert!(is_model_allowed(&allow, "claude-sonnet-4"));
    // A bare ID without glob is still interpreted as a pattern.
    // Since "claude-sonnet-4" contains no "*", it matches literally.
    let allow2 = ModelIdAllow {
        mode: ModelIdMatchMode::Pattern,
        allow: vec!["claude-sonnet-4".to_string()],
    };
    assert!(is_model_allowed(&allow2, "claude-sonnet-4"));
    assert!(!is_model_allowed(&allow2, "claude-opus-4"));
}

#[test]
fn glob_match_handles_prefix_suffix() {
    assert!(glob_match("claude-*", "claude-sonnet-4"));
    assert!(glob_match("claude-*-4", "claude-sonnet-4"));
    assert!(!glob_match("claude-*-4", "claude-sonnet-3"));
    assert!(glob_match("*", "anything"));
    assert!(glob_match("exact", "exact"));
    assert!(!glob_match("exact", "different"));
    assert!(glob_match("prefix-*", "prefix-suffix"));
    assert!(glob_match("*-suffix", "mid-suffix"));
}

#[test]
fn empty_allowlist_rejects_all() {
    let allow = ModelIdAllow {
        mode: ModelIdMatchMode::ExactOrPattern,
        allow: vec![],
    };
    assert!(!is_model_allowed(&allow, "anything"));
}

#[test]
fn manifest_finds_provider_by_models_dev_id() {
    let manifest = ProviderMappingManifest {
        schema_version: 1,
        providers: vec![ProviderMappingEntry {
            models_dev_id: "anthropic".to_string(),
            holon_provider_id: "anthropic".to_string(),
            kind: ProviderKind::Direct,
            transport: "anthropic_messages".to_string(),
            route_registration: "anthropic@default".to_string(),
            model_id: ModelIdAllow {
                mode: ModelIdMatchMode::ExactOrPattern,
                allow: vec!["claude-*".to_string()],
            },
            capability_ceiling: CapabilityCeiling::default(),
            limit_ceiling: LimitCeiling::default(),
            credential_ref: "anthropic".to_string(),
            enabled: false,
            provenance: MappingProvenance::default(),
        }],
    };
    assert!(manifest.find_provider("anthropic").is_some());
    assert!(manifest.find_provider("openai").is_none());
}
