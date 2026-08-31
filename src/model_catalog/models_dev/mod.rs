//! `models.dev` upstream adapter: DTO, projection, and artifact generation.
//!
//! This module implements Phase 2A+2B of the models.dev integration:
//!
//! - [`dto`] defines an independent upstream DTO that mirrors the
//!   `models.dev` JSON schema with tri-state `Option<T>` fields.
//! - [`projection`] maps DTO fields to Holon `BuiltInModelMetadata` using an
//!   explicit provider mapping. Capabilities can only narrow; missing fields
//!   stay `unknown`.
//! - [`artifact`] wraps the projected data with provenance metadata
//!   (upstream revision, content SHA-256, adapter version).
//!
//! The runtime does not consume `models.dev` directly. CI uses this module
//! to generate an immutable artifact that enters Holon through review and
//! merge.

pub mod artifact;
pub mod dto;
pub mod projection;

pub use artifact::{
    ArtifactAlias, ArtifactBuilder, ArtifactPreferredModel, ArtifactPreferredModelRoute,
    ArtifactPreferredRoute, ArtifactProvenance, ArtifactRoute, ModelsDevArtifact,
};
pub use dto::{
    ModelsDevCost, ModelsDevInterleaved, ModelsDevLimit, ModelsDevModalities, ModelsDevModel,
    ModelsDevProvider, ModelsDevReasoningOption, ModelsDevSnapshot,
};
pub use projection::{ProjectedModel, ProjectionResult, Projector, ProviderMapping, UnmappedModel};

/// Parses a `models.dev` JSON snapshot from raw bytes.
pub fn parse_snapshot(raw: &str) -> Result<ModelsDevSnapshot, serde_json::Error> {
    serde_json::from_str(raw)
}

/// End-to-end: parse, project, and build an artifact from raw `models.dev` JSON.
///
/// `upstream_revision` is the pinned revision identifier (e.g. git SHA).
/// `fetched_at` is an ISO-8601 timestamp.
/// `adapter_version` is the Holon crate version.
pub fn generate_artifact(
    raw_json: &str,
    upstream_revision: &str,
    fetched_at: &str,
    adapter_version: &str,
) -> Result<GeneratedArtifact, String> {
    let snapshot = parse_snapshot(raw_json)
        .map_err(|e| format!("failed to parse models.dev snapshot: {e}"))?;
    let result = Projector::new().project(&snapshot)?;
    let artifact = ArtifactBuilder::new(
        upstream_revision,
        fetched_at,
        raw_json.as_bytes(),
        adapter_version,
    )
    .build(&result);
    artifact.validate()?;
    Ok(GeneratedArtifact {
        artifact,
        projection: result,
    })
}

/// The output of [`generate_artifact`]: the validated artifact plus the
/// raw projection result (including unmapped models).
#[derive(Debug, Clone)]
pub struct GeneratedArtifact {
    pub artifact: ModelsDevArtifact,
    pub projection: ProjectionResult,
}
