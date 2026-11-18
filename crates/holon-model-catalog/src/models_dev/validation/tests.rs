use super::super::mapping::{
    CapabilityCeiling, LimitCeiling, MappingProvenance, ModelIdAllow, ModelIdMatchMode,
};
use super::*;
use crate::models_dev::dto::{
    ModelsDevLimit, ModelsDevModalities, ModelsDevModel, ModelsDevProvider,
};
use std::collections::BTreeMap;

/// A valid Anthropic baseline manifest entry.
fn anthropic_entry() -> ProviderMappingEntry {
    ProviderMappingEntry {
        models_dev_id: "anthropic".to_string(),
        holon_provider_id: "anthropic".to_string(),
        kind: ProviderKind::Direct,
        transport: "anthropic_messages".to_string(),
        route_registration: "anthropic@default".to_string(),
        model_id: ModelIdAllow {
            mode: ModelIdMatchMode::ExactOrPattern,
            allow: vec!["claude-*".to_string()],
        },
        capability_ceiling: CapabilityCeiling {
            tool_calling: true,
            image_input: true,
            image_generation: false,
            reasoning: true,
            structured_output: false,
        },
        limit_ceiling: LimitCeiling {
            context_window_tokens: Some(200_000),
            max_output_tokens: Some(32_000),
        },
        credential_ref: "anthropic".to_string(),
        enabled: false,
        provenance: MappingProvenance {
            owner: "holon".to_string(),
            reviewed_at: Some("2026-08-31".to_string()),
        },
    }
}

fn simple_manifest(entry: ProviderMappingEntry) -> ProviderMappingManifest {
    ProviderMappingManifest {
        schema_version: MAPPING_SCHEMA_VERSION,
        providers: vec![entry],
    }
}

/// Minimal models.dev snapshot with one Anthropic model.
fn anthropic_snapshot() -> ModelsDevSnapshot {
    let mut providers = BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        ModelsDevProvider {
            id: "anthropic".to_string(),
            env: vec![],
            npm: None,
            api: None,
            name: None,
            doc: None,
            models: {
                let mut m = BTreeMap::new();
                m.insert(
                    "claude-sonnet-4-20250514".to_string(),
                    ModelsDevModel {
                        id: "claude-sonnet-4-20250514".to_string(),
                        name: Some("Claude Sonnet 4".to_string()),
                        description: None,
                        family: None,
                        attachment: Some(true),
                        reasoning: Some(true),
                        reasoning_options: vec![],
                        tool_call: Some(true),
                        structured_output: None,
                        temperature: None,
                        knowledge: None,
                        release_date: None,
                        last_updated: None,
                        modalities: Some(ModelsDevModalities {
                            input: vec!["text".to_string(), "image".to_string()],
                            output: vec!["text".to_string()],
                        }),
                        open_weights: None,
                        limit: Some(ModelsDevLimit {
                            context: Some(200_000),
                            output: Some(8_192),
                            input: None,
                        }),
                        cost: None,
                        interleaved: None,
                    },
                );
                m
            },
        },
    );
    providers.insert(
        "unknown-provider".to_string(),
        ModelsDevProvider {
            id: "unknown-provider".to_string(),
            env: vec![],
            npm: None,
            api: None,
            name: None,
            doc: None,
            models: {
                let mut m = BTreeMap::new();
                m.insert(
                    "mystery-model".to_string(),
                    ModelsDevModel {
                        id: "mystery-model".to_string(),
                        name: Some("Mystery".to_string()),
                        description: None,
                        family: None,
                        attachment: None,
                        reasoning: None,
                        reasoning_options: vec![],
                        tool_call: None,
                        structured_output: None,
                        temperature: None,
                        knowledge: None,
                        release_date: None,
                        last_updated: None,
                        modalities: None,
                        open_weights: None,
                        limit: None,
                        cost: None,
                        interleaved: None,
                    },
                );
                m
            },
        },
    );
    ModelsDevSnapshot { providers }
}

// -- Anthropic baseline acceptance --

#[test]
fn validates_anthropic_baseline_without_errors() {
    let engine = ValidationEngine::new();
    let manifest = simple_manifest(anthropic_entry());
    let snapshot = anthropic_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));

    assert!(
        !report.has_errors(),
        "expected no errors, got: {:?}",
        report
            .entries
            .iter()
            .filter(|e| e.severity == ValidationSeverity::Error)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.mapped_providers, 1);
    assert_eq!(report.total_upstream_providers, 2);
    assert_eq!(report.unmapped_providers, vec!["unknown-provider"]);
    // Should have generated an offering record for the matched model
    assert_eq!(report.offerings.len(), 1);
    assert_eq!(
        report.offerings[0].model_identity,
        "claude-sonnet-4-20250514"
    );
    assert_eq!(report.offerings[0].callability, Callability::DiscoveryOnly);
    // Should report disabled mapping as info
    assert!(report.entries.iter().any(|e| e.code == "mapping_disabled"));
}

#[test]
fn anthropic_baseline_fixture_parses_and_validates() {
    let raw = include_str!("../manifests/anthropic_v1.json");
    let manifest = ProviderMappingManifest::parse(raw).unwrap();
    let engine = ValidationEngine::new();
    let report = engine.validate(&manifest, None);
    assert!(!report.has_errors());
}

// -- Rejection tests --

#[test]
fn rejects_unsupported_schema_version() {
    let engine = ValidationEngine::new();
    let mut manifest = simple_manifest(anthropic_entry());
    manifest.schema_version = 99;
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "unsupported_schema_version"));
}

#[test]
fn rejects_duplicate_models_dev_id() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    // Create a duplicate with a different holon_provider_id to avoid
    // triggering the conflicting_holon_provider_id check.
    entry.holon_provider_id = "anthropic-alt".to_string();
    let manifest = ProviderMappingManifest {
        schema_version: MAPPING_SCHEMA_VERSION,
        providers: vec![anthropic_entry(), entry],
    };
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "duplicate_models_dev_id"));
}

#[test]
fn rejects_conflicting_holon_provider_id() {
    let engine = ValidationEngine::new();
    let mut entry2 = anthropic_entry();
    entry2.models_dev_id = "anthropic-clone".to_string();
    entry2.transport = "openai_chat_completions".to_string();
    entry2.kind = ProviderKind::OpenAiCompatible;
    let manifest = ProviderMappingManifest {
        schema_version: MAPPING_SCHEMA_VERSION,
        providers: vec![anthropic_entry(), entry2],
    };
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "conflicting_holon_provider_id"));
}

#[test]
fn rejects_missing_route_registration() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    entry.route_registration = "nonexistent@default".to_string();
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "missing_route_registration"));
}

#[test]
fn rejects_transport_mismatch() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    entry.transport = "openai_chat_completions".to_string();
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "transport_mismatch"));
}

#[test]
fn rejects_kind_transport_mismatch() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    // Direct provider but with an OpenAI-compatible transport would
    // fail transport_mismatch first; use gateway kind to isolate
    // kind_transport_mismatch.
    entry.kind = ProviderKind::OpenAiCompatible;
    entry.transport = "anthropic_messages".to_string();
    // Make route_registration match by using a route with anthropic_messages
    // but claim openai_compatible kind — the transport check passes but
    // kind/transport mismatch should fire.
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "kind_transport_mismatch"));
}

#[test]
fn rejects_unknown_credential_ref() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    entry.credential_ref = "totally-unknown".to_string();
    // Also need to fix the credential_envs check — credential_ref must
    // match either the legacy_provider or one of the credential_envs.
    // "totally-unknown" should not match "anthropic" or "ANTHROPIC_AUTH_TOKEN".
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "unknown_credential_ref"));
}

#[test]
fn warns_on_model_outside_allowlist() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    entry.model_id.allow = vec!["claude-opus-*".to_string()];
    let manifest = simple_manifest(entry);
    let snapshot = anthropic_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(!report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "model_outside_allowlist" && e.severity == ValidationSeverity::Warning));
    // The model should not appear in offerings
    assert!(report.offerings.is_empty());
}

#[test]
fn warns_on_capability_widening() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    // Set image_input ceiling to false; the snapshot has image in input modalities
    entry.capability_ceiling.image_input = false;
    // Set reasoning ceiling to false; the snapshot has reasoning=true
    entry.capability_ceiling.reasoning = false;
    // Set tool_calling ceiling to false; the snapshot has tool_call=true
    entry.capability_ceiling.tool_calling = false;
    let manifest = simple_manifest(entry);
    let snapshot = anthropic_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(!report.has_errors());
    let codes: Vec<&str> = report.entries.iter().map(|e| e.code.as_str()).collect();
    assert!(codes.contains(&"capability_widening_image_input"));
    assert!(codes.contains(&"capability_widening_reasoning"));
    assert!(codes.contains(&"capability_widening_tool_calling"));
}

#[test]
fn warns_on_limit_exceeding_ceiling() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    entry.limit_ceiling.context_window_tokens = Some(100_000);
    entry.limit_ceiling.max_output_tokens = Some(4_096);
    let manifest = simple_manifest(entry);
    let snapshot = anthropic_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(!report.has_errors());
    let codes: Vec<&str> = report.entries.iter().map(|e| e.code.as_str()).collect();
    assert!(codes.contains(&"limit_exceeds_ceiling_context"));
    assert!(codes.contains(&"limit_exceeds_ceiling_output"));
}

#[test]
fn reports_unmapped_providers() {
    let engine = ValidationEngine::new();
    let manifest = simple_manifest(anthropic_entry());
    let snapshot = anthropic_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(report
        .unmapped_providers
        .contains(&"unknown-provider".to_string()));
}

#[test]
fn warns_on_incomplete_provenance() {
    let engine = ValidationEngine::new();
    let mut entry = anthropic_entry();
    entry.provenance.reviewed_at = None;
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "incomplete_provenance" && e.severity == ValidationSeverity::Warning));
}

#[test]
fn disabled_mapping_is_info_not_error() {
    let engine = ValidationEngine::new();
    let entry = anthropic_entry();
    assert!(!entry.enabled);
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(!report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "mapping_disabled" && e.severity == ValidationSeverity::Info));
}

#[test]
fn validates_without_snapshot() {
    let engine = ValidationEngine::new();
    let manifest = simple_manifest(anthropic_entry());
    let report = engine.validate(&manifest, None);
    assert!(!report.has_errors());
    assert_eq!(report.total_upstream_providers, 0);
    assert!(report.unmapped_providers.is_empty());
    assert!(report.offerings.is_empty());
}

// -- DeepSeek baseline (Phase 3B: OpenAI-compatible) --

/// A valid DeepSeek baseline manifest entry using the derived
/// `deepseek@responses` route with `openai_responses` transport.
fn deepseek_entry() -> ProviderMappingEntry {
    ProviderMappingEntry {
        models_dev_id: "deepseek".to_string(),
        holon_provider_id: "deepseek".to_string(),
        kind: ProviderKind::OpenAiCompatible,
        transport: "openai_responses".to_string(),
        route_registration: "deepseek@responses".to_string(),
        model_id: ModelIdAllow {
            mode: ModelIdMatchMode::ExactOrPattern,
            allow: vec![
                "deepseek-v4-pro".to_string(),
                "deepseek-v4-flash".to_string(),
            ],
        },
        capability_ceiling: CapabilityCeiling {
            tool_calling: true,
            image_input: false,
            image_generation: false,
            reasoning: true,
            structured_output: false,
        },
        limit_ceiling: LimitCeiling {
            context_window_tokens: Some(1_000_000),
            max_output_tokens: Some(384_000),
        },
        credential_ref: "deepseek".to_string(),
        enabled: false,
        provenance: MappingProvenance {
            owner: "holon".to_string(),
            reviewed_at: Some("2026-08-31".to_string()),
        },
    }
}

/// Minimal models.dev snapshot with one DeepSeek model.
fn deepseek_snapshot() -> ModelsDevSnapshot {
    let mut providers = BTreeMap::new();
    providers.insert(
        "deepseek".to_string(),
        ModelsDevProvider {
            id: "deepseek".to_string(),
            env: vec!["DEEPSEEK_API_KEY".to_string()],
            npm: None,
            api: None,
            name: None,
            doc: None,
            models: {
                let mut m = BTreeMap::new();
                m.insert(
                    "deepseek-v4-flash".to_string(),
                    ModelsDevModel {
                        id: "deepseek-v4-flash".to_string(),
                        name: Some("DeepSeek V4 Flash".to_string()),
                        description: None,
                        family: None,
                        attachment: Some(false),
                        reasoning: Some(true),
                        reasoning_options: vec![],
                        tool_call: Some(true),
                        structured_output: Some(true),
                        temperature: None,
                        knowledge: None,
                        release_date: None,
                        last_updated: None,
                        modalities: Some(ModelsDevModalities {
                            input: vec!["text".to_string()],
                            output: vec!["text".to_string()],
                        }),
                        open_weights: None,
                        limit: Some(ModelsDevLimit {
                            context: Some(1_000_000),
                            output: Some(384_000),
                            input: None,
                        }),
                        cost: None,
                        interleaved: None,
                    },
                );
                m
            },
        },
    );
    ModelsDevSnapshot { providers }
}

#[test]
fn validates_deepseek_baseline_without_errors() {
    let engine = ValidationEngine::new();
    let manifest = simple_manifest(deepseek_entry());
    let snapshot = deepseek_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));

    assert!(
        !report.has_errors(),
        "expected no errors, got: {:?}",
        report
            .entries
            .iter()
            .filter(|e| e.severity == ValidationSeverity::Error)
            .collect::<Vec<_>>()
    );
    assert_eq!(report.mapped_providers, 1);
    assert_eq!(report.offerings.len(), 1);
    assert_eq!(report.offerings[0].model_identity, "deepseek-v4-flash");
    assert_eq!(report.offerings[0].route_registration, "deepseek@responses");
    assert_eq!(report.offerings[0].callability, Callability::DiscoveryOnly);
    // Should report disabled mapping as info
    assert!(report.entries.iter().any(|e| e.code == "mapping_disabled"));
    // structured_output=true upstream but ceiling=false → warning
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "capability_widening_structured_output"));
}

#[test]
fn deepseek_baseline_fixture_parses_and_validates() {
    let raw = include_str!("../manifests/deepseek_v1.json");
    let manifest = ProviderMappingManifest::parse(raw).unwrap();
    let engine = ValidationEngine::new();
    let report = engine.validate(&manifest, None);
    assert!(!report.has_errors());
    // Should report disabled mapping as info
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "mapping_disabled" && e.severity == ValidationSeverity::Info));
}

#[test]
fn deepseek_manifest_permits_flash_vision_without_allowing_similar_ids() {
    let raw = include_str!("../manifests/deepseek_v1.json");
    let mut manifest = ProviderMappingManifest::parse(raw).unwrap();
    let mut snapshot = deepseek_snapshot();
    let models = &mut snapshot.providers.get_mut("deepseek").unwrap().models;
    let mut flash = models.get("deepseek-v4-flash").unwrap().clone();
    flash.id = "deepseek-flash".into();
    flash.name = Some("DeepSeek Flash".into());
    flash.attachment = Some(true);
    flash
        .modalities
        .as_mut()
        .unwrap()
        .input
        .push("image".into());
    flash.limit = None;
    models.insert(flash.id.clone(), flash.clone());
    flash.id = "deepseek-flash-preview".into();
    models.insert(flash.id.clone(), flash);

    let engine = ValidationEngine::new();
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(!report.has_errors());
    assert_eq!(report.offerings.len(), 2);
    assert!(report.offerings.iter().any(|offering| {
        offering.model_identity == "deepseek-flash"
            && offering.route_registration == "deepseek@responses"
            && offering.callability == Callability::DiscoveryOnly
    }));
    assert!(!report
        .offerings
        .iter()
        .any(|offering| offering.model_identity == "deepseek-flash-preview"));
    assert!(!report
        .entries
        .iter()
        .any(|entry| entry.code == "capability_widening_image_input"));
    assert!(report.entries.iter().any(|entry| {
        entry.code == "model_outside_allowlist"
            && entry.entry_identity.as_deref() == Some("deepseek/deepseek-flash-preview")
    }));

    // A closed ceiling would suppress Flash's documented vision.
    manifest.providers[0].capability_ceiling.image_input = false;
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(report.entries.iter().any(|entry| {
        entry.code == "capability_widening_image_input"
            && entry.entry_identity.as_deref() == Some("deepseek/deepseek-flash")
    }));
}

#[test]
fn rejects_deepseek_transport_mismatch() {
    let engine = ValidationEngine::new();
    let mut entry = deepseek_entry();
    // deepseek@responses route uses OpenAiResponses transport;
    // claiming anthropic_messages must fail.
    entry.transport = "anthropic_messages".to_string();
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "transport_mismatch"));
}

#[test]
fn rejects_deepseek_unknown_credential_ref() {
    let engine = ValidationEngine::new();
    let mut entry = deepseek_entry();
    entry.credential_ref = "totally-unknown".to_string();
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "unknown_credential_ref"));
}

#[test]
fn rejects_deepseek_missing_route_registration() {
    let engine = ValidationEngine::new();
    let mut entry = deepseek_entry();
    entry.route_registration = "deepseek@nonexistent".to_string();
    let manifest = simple_manifest(entry);
    let report = engine.validate(&manifest, None);
    assert!(report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "missing_route_registration"));
}

#[test]
fn deepseek_model_outside_allowlist_excluded() {
    let engine = ValidationEngine::new();
    let mut entry = deepseek_entry();
    // Narrow allowlist to only pro; flash should be excluded.
    entry.model_id.allow = vec!["deepseek-v4-pro".to_string()];
    let manifest = simple_manifest(entry);
    let snapshot = deepseek_snapshot();
    let report = engine.validate(&manifest, Some(&snapshot));
    assert!(!report.has_errors());
    assert!(report
        .entries
        .iter()
        .any(|e| e.code == "model_outside_allowlist" && e.severity == ValidationSeverity::Warning));
    assert!(report.offerings.is_empty());
}
