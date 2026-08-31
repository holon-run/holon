//! Artifact generation for `models.dev` projections.
//!
//! The artifact wraps the projected snapshot with provenance metadata:
//! upstream source, revision, fetched-at timestamp, content SHA-256, and
//! adapter version. The artifact is a CI intermediate; the runtime does not
//! load it directly in this phase.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Encode bytes as lowercase hex without external dependency.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
use crate::config::{ModelRef, ModelRouteRef, ProviderId};
use crate::model_catalog::BuiltInModelMetadata;

use super::projection::ProjectionResult;

/// The schema version for models.dev artifacts.
const ARTIFACT_SCHEMA_VERSION: u32 = 1;

/// Provenance metadata for a models.dev artifact.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactProvenance {
    /// The upstream source identifier.
    pub source: String,
    /// The pinned upstream revision (git SHA, tag, or timestamp).
    pub revision: String,
    /// When the upstream data was fetched (ISO-8601).
    pub fetched_at: String,
    /// SHA-256 digest of the raw upstream content.
    pub content_sha256: String,
    /// Version of the Holon adapter that generated this artifact.
    pub adapter_version: String,
}

/// An immutable artifact wrapping projected models.dev data.
///
/// The `models`, `routes`, `aliases`, and `preferred_*` fields use the same
/// format as the built-in registry snapshot. `routes`, `aliases`, and
/// `preferred_*` are empty because endpoint, route, and default selections
/// are Holon-controlled and not derivable from `models.dev` metadata alone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelsDevArtifact {
    pub schema_version: u32,
    pub revision: String,
    pub upstream: ArtifactProvenance,
    pub models: Vec<BuiltInModelMetadata>,
    #[serde(default)]
    pub routes: Vec<ArtifactRoute>,
    #[serde(default)]
    pub aliases: Vec<ArtifactAlias>,
    #[serde(default)]
    pub preferred_models: Vec<ArtifactPreferredModel>,
    #[serde(default)]
    pub preferred_routes: Vec<ArtifactPreferredRoute>,
    #[serde(default)]
    pub preferred_routes_by_model: Vec<ArtifactPreferredModelRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRoute {
    pub route_ref: ModelRouteRef,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactAlias {
    pub alias: ModelRef,
    pub target: ModelRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactPreferredModel {
    pub provider: ProviderId,
    pub model_ref: ModelRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactPreferredRoute {
    pub provider: ProviderId,
    pub route_ref: ModelRouteRef,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactPreferredModelRoute {
    pub model_ref: ModelRef,
    pub route_ref: ModelRouteRef,
}

/// Builder for constructing a `ModelsDevArtifact` from a projection result.
pub struct ArtifactBuilder {
    upstream_source: String,
    upstream_revision: String,
    fetched_at: String,
    raw_content: Vec<u8>,
    adapter_version: String,
}

impl ArtifactBuilder {
    /// Creates a new builder with the given provenance fields.
    ///
    /// `raw_content` is the original upstream bytes used to compute the
    /// content SHA-256 digest.
    pub fn new(
        upstream_revision: impl Into<String>,
        fetched_at: impl Into<String>,
        raw_content: impl Into<Vec<u8>>,
        adapter_version: impl Into<String>,
    ) -> Self {
        Self {
            upstream_source: "models.dev".to_string(),
            upstream_revision: upstream_revision.into(),
            fetched_at: fetched_at.into(),
            raw_content: raw_content.into(),
            adapter_version: adapter_version.into(),
        }
    }

    /// Builds the artifact from a projection result.
    pub fn build(self, result: &ProjectionResult) -> ModelsDevArtifact {
        let content_sha256 = hex_encode(Sha256::digest(&self.raw_content).as_slice());
        let revision = format!("models-dev-{}", self.upstream_revision);

        let models: Vec<BuiltInModelMetadata> = result
            .projected
            .iter()
            .map(|pm| pm.metadata.clone())
            .collect();

        ModelsDevArtifact {
            schema_version: ARTIFACT_SCHEMA_VERSION,
            revision,
            upstream: ArtifactProvenance {
                source: self.upstream_source,
                revision: self.upstream_revision,
                fetched_at: self.fetched_at,
                content_sha256,
                adapter_version: self.adapter_version,
            },
            models,
            routes: Vec::new(),
            aliases: Vec::new(),
            preferred_models: Vec::new(),
            preferred_routes: Vec::new(),
            preferred_routes_by_model: Vec::new(),
        }
    }
}

impl ModelsDevArtifact {
    /// Serializes the artifact to pretty-printed JSON.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Computes the SHA-256 digest of the artifact JSON.
    pub fn content_sha256(&self) -> String {
        let json = self
            .to_json()
            .expect("artifact serialization must not fail");
        hex_encode(Sha256::digest(json.as_bytes()).as_slice())
    }

    /// Returns the number of models in the artifact.
    pub fn model_count(&self) -> usize {
        self.models.len()
    }

    /// Validates the artifact's internal consistency.
    ///
    /// Checks that:
    /// - schema_version is the expected version;
    /// - revision and upstream fields are non-empty;
    /// - model identities are unique;
    /// - token limits are positive when present.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != ARTIFACT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported artifact schema version {}; expected {ARTIFACT_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.revision.trim().is_empty() {
            return Err("artifact revision must not be empty".to_string());
        }
        if self.upstream.revision.trim().is_empty() {
            return Err("upstream revision must not be empty".to_string());
        }
        if self.upstream.content_sha256.trim().is_empty() {
            return Err("upstream content_sha256 must not be empty".to_string());
        }

        let mut seen = std::collections::HashSet::new();
        for model in &self.models {
            let key = model.model_ref.as_string();
            if !seen.insert(key.clone()) {
                return Err(format!("duplicate model {key}"));
            }
            if model.context_window_tokens.is_some_and(|v| v == 0) {
                return Err(format!("model {key} has zero context window"));
            }
            if model.default_max_output_tokens.is_some_and(|v| v == 0) {
                return Err(format!("model {key} has zero default max output tokens"));
            }
            if let (Some(default), Some(upper)) = (
                model.default_max_output_tokens,
                model.max_output_tokens_upper_limit,
            ) {
                if default > upper {
                    return Err(format!(
                        "model {key} default max output {default} exceeds upper limit {upper}"
                    ));
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
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
}
