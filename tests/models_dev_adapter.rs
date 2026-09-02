//! Integration tests for the models.dev upstream adapter (Phase 2A+2B).
//!
//! These tests verify the end-to-end flow: parse a `models.dev` fixture,
//! project it into Holon canonical model metadata, build an immutable
//! artifact with provenance, and validate internal consistency.

use std::fs;

use holon::model_catalog::models_dev::{
    generate_artifact, parse_snapshot, ArtifactBuilder, ModelsDevArtifact, Projector,
};

const FIXTURE_PATH: &str = "tests/fixtures/models_dev/sample.json";

fn fixture_json() -> String {
    fs::read_to_string(FIXTURE_PATH).expect("fixture must be readable")
}

#[test]
fn parse_fixture_snapshot() {
    let json = fixture_json();
    let snapshot = parse_snapshot(&json).expect("parse must succeed");
    assert!(snapshot.providers.contains_key("openai"));
    assert!(snapshot.providers.contains_key("anthropic"));
    assert!(snapshot.providers.contains_key("hpc-ai"));
    assert!(snapshot.providers.contains_key("zai-org"));
}

#[test]
fn project_fixture_with_default_mappings() {
    let json = fixture_json();
    let snapshot = parse_snapshot(&json).unwrap();
    let result = Projector::new().project(&snapshot).unwrap();

    // openai and anthropic are mapped; hpc-ai and zai-org are not.
    assert!(result.projected.len() >= 3); // 2 openai + 1 anthropic
    assert_eq!(result.unmapped.len(), 2); // 1 hpc-ai + 1 zai-org
}

#[test]
fn projected_model_has_correct_capabilities() {
    let json = fixture_json();
    let snapshot = parse_snapshot(&json).unwrap();
    let result = Projector::new().project(&snapshot).unwrap();

    let gpt55 = result
        .projected
        .iter()
        .find(|m| m.models_dev_model_id == "gpt-5.5")
        .expect("gpt-5.5 must be projected");

    assert!(gpt55.metadata.capabilities.supports_reasoning);
    assert!(gpt55.metadata.capabilities.image_input);
    assert!(!gpt55.metadata.capabilities.image_generation);
    assert_eq!(
        gpt55.metadata.reasoning_effort_options,
        vec!["none", "low", "medium", "high"]
    );
    assert_eq!(gpt55.metadata.context_window_tokens, Some(1_050_000));
    assert_eq!(gpt55.metadata.default_max_output_tokens, Some(128_000));
}

#[test]
fn projected_image_generation_model() {
    let json = fixture_json();
    let snapshot = parse_snapshot(&json).unwrap();
    let result = Projector::new().project(&snapshot).unwrap();

    let gpt4o = result
        .projected
        .iter()
        .find(|m| m.models_dev_model_id == "gpt-4o-mini")
        .expect("gpt-4o-mini must be projected");

    assert!(!gpt4o.metadata.capabilities.supports_reasoning);
    assert!(gpt4o.metadata.capabilities.image_generation);
}

#[test]
fn unmapped_providers_are_skipped() {
    let json = fixture_json();
    let snapshot = parse_snapshot(&json).unwrap();
    let result = Projector::new().project(&snapshot).unwrap();

    assert!(result
        .unmapped
        .iter()
        .any(|u| u.models_dev_provider_id == "hpc-ai"));
    assert!(result
        .unmapped
        .iter()
        .any(|u| u.models_dev_provider_id == "zai-org"));
}

#[test]
fn generate_artifact_end_to_end() {
    let json = fixture_json();
    let generated = generate_artifact(
        &json,
        "fixture-rev-001",
        "2026-08-31T00:00:00Z",
        "test-0.1.0",
    )
    .expect("artifact generation must succeed");

    assert_eq!(generated.artifact.schema_version, 1);
    assert_eq!(generated.artifact.revision, "models-dev-fixture-rev-001");
    assert_eq!(generated.artifact.upstream.source, "models.dev");
    assert_eq!(generated.artifact.upstream.revision, "fixture-rev-001");
    assert!(!generated.artifact.upstream.content_sha256.is_empty());
    assert!(generated.artifact.model_count() >= 3);
    assert!(generated.artifact.routes.is_empty());
    assert!(generated.artifact.aliases.is_empty());
    assert!(generated.artifact.preferred_models.is_empty());
}

#[test]
fn artifact_json_roundtrips_through_serde() {
    let json = fixture_json();
    let generated = generate_artifact(
        &json,
        "fixture-rev-001",
        "2026-08-31T00:00:00Z",
        "test-0.1.0",
    )
    .unwrap();
    let serialized = generated.artifact.to_json().unwrap();
    let parsed: ModelsDevArtifact = serde_json::from_str(&serialized).unwrap();
    assert_eq!(parsed, generated.artifact);
}

#[test]
fn artifact_treats_zero_upstream_limits_as_unknown() {
    let json = r#"{
        "openai": {
            "id": "openai",
            "models": {
                "zero-limits-model": {
                    "id": "zero-limits-model",
                    "limit": {"context": 0, "output": 128000}
                }
            }
        }
    }"#;
    let snapshot = parse_snapshot(json).unwrap();
    let result = Projector::new().project(&snapshot).unwrap();
    let model = result
        .projected
        .iter()
        .find(|m| m.models_dev_model_id == "zero-limits-model")
        .unwrap();
    // Upstream reports 0 limits for some models; projection must treat them
    // as unknown rather than emitting invalid token limits.
    assert_eq!(model.metadata.context_window_tokens, None);
    let artifact = ArtifactBuilder::new("rev", "ts", b"raw", "v").build(&result);
    assert!(artifact.validate().is_ok());
}

#[test]
fn schema_drift_unknown_fields_ignored() {
    let json = r#"{
        "openai": {
            "id": "openai",
            "future_provider_field": "ignored",
            "models": {
                "gpt-5.5": {
                    "id": "gpt-5.5",
                    "name": "GPT-5.5",
                    "future_model_field": 42,
                    "reasoning": true
                }
            }
        }
    }"#;
    let snapshot = parse_snapshot(json).expect("unknown fields must not break parsing");
    let result = Projector::new().project(&snapshot).unwrap();
    assert_eq!(result.projected.len(), 1);
    assert!(result.projected[0].metadata.capabilities.supports_reasoning);
}

#[test]
fn missing_fields_do_not_become_true() {
    let json = r#"{
        "openai": {
            "id": "openai",
            "models": {
                "minimal": {
                    "id": "minimal"
                }
            }
        }
    }"#;
    let snapshot = parse_snapshot(json).unwrap();
    let result = Projector::new().project(&snapshot).unwrap();

    let model = &result.projected[0].metadata;
    assert!(!model.capabilities.supports_reasoning);
    assert!(!model.capabilities.image_input);
    assert!(!model.capabilities.image_generation);
    assert!(model.context_window_tokens.is_none());
    assert!(model.default_max_output_tokens.is_none());
}

#[test]
fn custom_provider_mapping_projects_unmapped() {
    let json = fixture_json();
    let snapshot = parse_snapshot(&json).unwrap();

    // Map zai-org to the Holon "zai" provider.
    use holon::model_catalog::models_dev::ProviderMapping;
    let mut mappings = ProviderMapping::default_mappings();
    mappings.push(ProviderMapping {
        models_dev_id: "zai-org".to_string(),
        holon_provider_id: "zai".to_string(),
    });

    let projector = Projector::with_mappings(mappings);
    let result = projector.project(&snapshot).unwrap();

    assert!(result.projected.iter().any(|m| {
        m.metadata.model_ref.provider.as_str() == "zai" && m.metadata.model_ref.model == "glm-5.2"
    }));
}
