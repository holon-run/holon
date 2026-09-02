//! `models.dev` upstream adapter: DTO, projection, and artifact generation.
//!
//! This module implements Phase 2A+2B (metadata ingestion), Phase 3A
//! (explicit provider mapping — Anthropic baseline), and Phase 3B
//! (OpenAI-compatible baseline — DeepSeek) of the models.dev integration:
//!
//! Also includes provider mapping audit for CI and manual review.
//!
//! - [`dto`] defines an independent upstream DTO that mirrors the
//!   `models.dev` JSON schema with tri-state `Option<T>` fields.
//! - [`projection`] maps DTO fields to Holon `BuiltInModelMetadata` using an
//!   explicit provider mapping. Capabilities can only narrow; missing fields
//!   stay `unknown`.
//! - [`artifact`] wraps the projected data with provenance metadata
//!   (upstream revision, content SHA-256, adapter version).
//! - [`mapping`] defines the versioned, Holon-owned provider mapping manifest
//!   schema that connects `models.dev` provider IDs to Holon route identities.
//! - [`supplement`] drafts new models for already-supported providers into a
//!   checked-in supplemental catalog that the runtime merges after review.
//! - [`validation`] validates a manifest against Holon's built-in provider
//!   definitions and optionally a `models.dev` snapshot, producing a
//!   deterministic report with rejection diagnostics.
//! - [`audit_mappings`] compares provider mappings against a live snapshot
//!   to surface unmapped upstream providers and stale Holon mappings.
//!
//! The runtime never fetches `models.dev` at run time. CI uses this module
//! to generate an immutable artifact and a supplemental catalog draft; both
//! enter Holon through review and merge.

pub mod artifact;
pub mod dto;
pub mod mapping;
pub mod projection;
pub mod supplement;
pub mod validation;

pub use artifact::{
    ArtifactAlias, ArtifactBuilder, ArtifactPreferredModel, ArtifactPreferredModelRoute,
    ArtifactPreferredRoute, ArtifactProvenance, ArtifactRoute, ModelsDevArtifact,
};
pub use dto::{
    ModelsDevCost, ModelsDevInterleaved, ModelsDevLimit, ModelsDevModalities, ModelsDevModel,
    ModelsDevProvider, ModelsDevReasoningOption, ModelsDevSnapshot,
};
pub use mapping::{
    Callability, CapabilityCeiling, LimitCeiling, MappingProvenance, ModelIdAllow,
    ModelIdMatchMode, OfferingRecord, ProviderKind, ProviderMappingEntry, ProviderMappingManifest,
    MAPPING_SCHEMA_VERSION,
};
pub use projection::{ProjectedModel, ProjectionResult, Projector, ProviderMapping, UnmappedModel};
pub use supplement::{
    render_summary_markdown, DeferredModel, DeferredReason, ModelsDevSupplement, RemovalReason,
    RemovedModel, SupplementUpdate, AUTO_SUPPLEMENT_PROVIDERS, RECENCY_WINDOW_DAYS,
};
pub use validation::{ValidationEngine, ValidationEntry, ValidationReport, ValidationSeverity};

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

/// A single mapped provider entry in a [`MappingAuditReport`].
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MappedProviderEntry {
    pub models_dev_id: String,
    pub holon_provider_id: String,
}

/// Report comparing Holon provider mappings against a `models.dev` snapshot.
///
/// Produced by [`audit_mappings`]. Used by CI and manual review to detect
/// unmapped upstream providers and stale Holon mappings.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct MappingAuditReport {
    pub mapped: Vec<MappedProviderEntry>,
    pub unmapped_upstream: Vec<String>,
    pub stale_holon: Vec<MappedProviderEntry>,
}

/// Audits provider mappings against a `models.dev` snapshot.
///
/// Reports mapped, unmapped upstream, and stale (dead) mappings.
/// Does not change runtime behavior.
pub fn audit_mappings(
    snapshot: &ModelsDevSnapshot,
    mappings: &[ProviderMapping],
) -> MappingAuditReport {
    use std::collections::BTreeSet;
    let snapshot_ids: BTreeSet<&str> = snapshot.providers.keys().map(String::as_str).collect();
    let mapping_md_ids: BTreeSet<&str> =
        mappings.iter().map(|m| m.models_dev_id.as_str()).collect();
    let mapped = mappings
        .iter()
        .filter(|m| snapshot_ids.contains(m.models_dev_id.as_str()))
        .map(|m| MappedProviderEntry {
            models_dev_id: m.models_dev_id.clone(),
            holon_provider_id: m.holon_provider_id.clone(),
        })
        .collect();
    let unmapped_upstream = snapshot_ids
        .iter()
        .filter(|id| !mapping_md_ids.contains(**id))
        .map(|id| id.to_string())
        .collect();
    let stale_holon = mappings
        .iter()
        .filter(|m| !snapshot_ids.contains(m.models_dev_id.as_str()))
        .map(|m| MappedProviderEntry {
            models_dev_id: m.models_dev_id.clone(),
            holon_provider_id: m.holon_provider_id.clone(),
        })
        .collect();
    MappingAuditReport {
        mapped,
        unmapped_upstream,
        stale_holon,
    }
}

#[cfg(test)]
mod audit_tests {
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
}
