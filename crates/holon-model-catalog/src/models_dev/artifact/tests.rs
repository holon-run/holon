use super::super::dto::ModelsDevSnapshot;
use super::super::projection::Projector;
use super::*;

fn sample_projection() -> ProjectionResult {
    let json = r#"{
        "openai": {
            "id": "openai",
            "models": {
                "gpt-5.5": {
                    "id": "gpt-5.5",
                    "name": "GPT-5.5",
                    "reasoning": true,
                    "modalities": {"input": ["text", "image"], "output": ["text"]},
                    "limit": {"context": 1050000, "output": 128000}
                }
            }
        }
    }"#;
    let snapshot: ModelsDevSnapshot = serde_json::from_str(json).unwrap();
    Projector::new().project(&snapshot).unwrap()
}

#[test]
fn build_artifact_from_projection() {
    let result = sample_projection();
    let raw = b"raw-upstream-content";
    let artifact =
        ArtifactBuilder::new("abc123", "2026-08-31T00:00:00Z", raw, "0.1.0").build(&result);

    assert_eq!(artifact.schema_version, 1);
    assert_eq!(artifact.revision, "models-dev-abc123");
    assert_eq!(artifact.upstream.source, "models.dev");
    assert_eq!(artifact.upstream.revision, "abc123");
    assert_eq!(artifact.upstream.fetched_at, "2026-08-31T00:00:00Z");
    assert!(!artifact.upstream.content_sha256.is_empty());
    assert_eq!(artifact.upstream.adapter_version, "0.1.0");
    assert_eq!(artifact.model_count(), 1);
    assert!(artifact.routes.is_empty());
    assert!(artifact.aliases.is_empty());
}

#[test]
fn artifact_validates() {
    let result = sample_projection();
    let artifact =
        ArtifactBuilder::new("abc123", "2026-08-31T00:00:00Z", b"raw", "0.1.0").build(&result);
    assert!(artifact.validate().is_ok());
}

#[test]
fn artifact_rejects_duplicate_models() {
    let result = sample_projection();
    let mut artifact =
        ArtifactBuilder::new("abc123", "2026-08-31T00:00:00Z", b"raw", "0.1.0").build(&result);
    // Duplicate the first model
    artifact.models.push(artifact.models[0].clone());
    assert!(artifact.validate().is_err());
}

#[test]
fn artifact_json_roundtrips() {
    let result = sample_projection();
    let artifact =
        ArtifactBuilder::new("abc123", "2026-08-31T00:00:00Z", b"raw", "0.1.0").build(&result);
    let json = artifact.to_json().unwrap();
    let parsed: ModelsDevArtifact = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, artifact);
}

#[test]
fn content_sha256_deterministic() {
    let result = sample_projection();
    let mk = || ArtifactBuilder::new("abc123", "2026-08-31T00:00:00Z", b"raw", "0.1.0");
    let a1 = mk().build(&result);
    let a2 = mk().build(&result);
    assert_eq!(a1.content_sha256(), a2.content_sha256());
}
