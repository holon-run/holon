//! Validation engine and report for provider mapping manifests (Phase 3A).
//!
//! The validator takes a [`ProviderMappingManifest`], optionally a
//! `models.dev` snapshot, and a snapshot of Holon's built-in provider
//! definitions. It produces a deterministic [`ValidationReport`] with
//! actionable diagnostics. An unmapped upstream provider is reportable
//! discovery data only; it cannot produce a callable route.
//!
//! See `docs/rfcs/models-dev-provider-mapping.md` for the normative contract.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::config::ProviderTransportKind;
use crate::provider::provider_definitions;

use super::dto::ModelsDevSnapshot;
use super::mapping::{
    is_model_allowed, Callability, OfferingRecord, ProviderKind, ProviderMappingEntry,
    ProviderMappingManifest, MAPPING_SCHEMA_VERSION,
};

/// Converts a `ProviderTransportKind` to its wire name string.
fn transport_wire_name(kind: ProviderTransportKind) -> &'static str {
    match kind {
        ProviderTransportKind::OpenAiCodexResponses => "openai_codex_responses",
        ProviderTransportKind::OpenAiResponses => "openai_responses",
        ProviderTransportKind::OpenAiChatCompletions => "openai_chat_completions",
        ProviderTransportKind::AnthropicMessages => "anthropic_messages",
        ProviderTransportKind::GeminiGenerateContent => "gemini_generate_content",
    }
}

/// A resolved route registration from Holon's built-in provider definitions.
#[derive(Debug, Clone)]
struct RouteRegistration {
    transport: ProviderTransportKind,
    credential_envs: Vec<String>,
    legacy_provider: String,
}

/// Severity of a validation diagnostic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValidationSeverity {
    Error,
    Warning,
    Info,
}

/// A single validation diagnostic entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationEntry {
    /// Machine-readable diagnostic code.
    pub code: String,
    /// Severity level.
    pub severity: ValidationSeverity,
    /// Human-readable message.
    pub message: String,
    /// Identity of the manifest entry or offering this diagnostic applies to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entry_identity: Option<String>,
    /// Source value that triggered the diagnostic.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_value: Option<String>,
    /// Effective value after applying Holon policy (if applicable).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_value: Option<String>,
}

/// The validation report produced by [`ValidationEngine::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Manifest revision identifier (if provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_revision: Option<String>,
    /// Upstream `models.dev` revision/hash (if a snapshot was provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream_revision: Option<String>,
    /// All validation diagnostics, sorted by severity (errors first).
    pub entries: Vec<ValidationEntry>,
    /// Upstream provider IDs not present in the manifest.
    pub unmapped_providers: Vec<String>,
    /// Total upstream providers (0 if no snapshot was provided).
    pub total_upstream_providers: usize,
    /// Mapped provider count.
    pub mapped_providers: usize,
    /// Validated offering records (always `discovery_only` in Phase 3A).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub offerings: Vec<OfferingRecord>,
}

impl ValidationReport {
    /// Returns `true` if the report contains any error-severity diagnostics.
    pub fn has_errors(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.severity == ValidationSeverity::Error)
    }

    /// Returns the count of error-severity diagnostics.
    pub fn error_count(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.severity == ValidationSeverity::Error)
            .count()
    }
}

/// The validation engine for provider mapping manifests.
///
/// Constructed from Holon's built-in provider definitions. The engine
/// resolves `route_registration` keys and checks transport/credential
/// compatibility deterministically.
pub struct ValidationEngine {
    routes: BTreeMap<String, RouteRegistration>,
}

impl ValidationEngine {
    /// Creates a validation engine from the built-in provider definitions.
    pub fn new() -> Self {
        let mut routes = BTreeMap::new();
        for def in provider_definitions() {
            let key = format!("{}@{}", def.route_provider, def.route_endpoint);
            routes.insert(
                key,
                RouteRegistration {
                    transport: def.transport,
                    credential_envs: def.credential_envs.iter().map(|s| s.to_string()).collect(),
                    legacy_provider: def.legacy_provider.to_string(),
                },
            );
        }
        Self { routes }
    }

    /// Validates a manifest, optionally cross-referencing a `models.dev` snapshot.
    pub fn validate(
        &self,
        manifest: &ProviderMappingManifest,
        snapshot: Option<&ModelsDevSnapshot>,
    ) -> ValidationReport {
        let mut entries = Vec::new();
        let mut unmapped_providers = Vec::new();
        let mut offerings = Vec::new();

        // 1. Schema version check
        if manifest.schema_version != MAPPING_SCHEMA_VERSION {
            entries.push(ValidationEntry {
                code: "unsupported_schema_version".to_string(),
                severity: ValidationSeverity::Error,
                message: format!(
                    "unsupported schema_version {}; expected {}",
                    manifest.schema_version, MAPPING_SCHEMA_VERSION
                ),
                entry_identity: None,
                source_value: Some(manifest.schema_version.to_string()),
                effective_value: Some(MAPPING_SCHEMA_VERSION.to_string()),
            });
        }

        // 2. Duplicate models_dev_id check
        let mut seen_md_ids: BTreeSet<&str> = BTreeSet::new();
        for entry in &manifest.providers {
            if !seen_md_ids.insert(&entry.models_dev_id) {
                entries.push(ValidationEntry {
                    code: "duplicate_models_dev_id".to_string(),
                    severity: ValidationSeverity::Error,
                    message: format!("duplicate models_dev_id: {}", entry.models_dev_id),
                    entry_identity: Some(entry.models_dev_id.clone()),
                    source_value: Some(entry.models_dev_id.clone()),
                    effective_value: None,
                });
            }
        }

        // 3. Conflicting holon_provider_id check (same ID, different kind/transport)
        let mut seen_holon: BTreeMap<&str, &ProviderMappingEntry> = BTreeMap::new();
        for entry in &manifest.providers {
            if let Some(existing) = seen_holon.get(entry.holon_provider_id.as_str()) {
                if existing.kind != entry.kind || existing.transport != entry.transport {
                    entries.push(ValidationEntry {
                        code: "conflicting_holon_provider_id".to_string(),
                        severity: ValidationSeverity::Error,
                        message: format!(
                            "conflicting holon_provider_id: {} has different kind or transport",
                            entry.holon_provider_id
                        ),
                        entry_identity: Some(entry.holon_provider_id.clone()),
                        source_value: Some(format!("{:?}/{}", entry.kind, entry.transport)),
                        effective_value: Some(format!(
                            "{:?}/{}",
                            existing.kind, existing.transport
                        )),
                    });
                }
            } else {
                seen_holon.insert(&entry.holon_provider_id, entry);
            }
        }

        // 4. Per-entry validation
        for entry in &manifest.providers {
            self.validate_entry(entry, &mut entries);
        }

        // 5. Cross-reference with models.dev snapshot
        let total_upstream = if let Some(snap) = snapshot {
            self.validate_against_snapshot(
                manifest,
                snap,
                &mut entries,
                &mut unmapped_providers,
                &mut offerings,
            );
            snap.providers.len()
        } else {
            0
        };

        // Sort: errors first, then warnings, then info
        entries.sort_by(|a, b| {
            a.severity
                .cmp(&b.severity)
                .then_with(|| a.code.cmp(&b.code))
        });

        ValidationReport {
            manifest_revision: None,
            upstream_revision: None,
            entries,
            unmapped_providers,
            total_upstream_providers: total_upstream,
            mapped_providers: manifest.providers.len(),
            offerings,
        }
    }

    fn validate_entry(&self, entry: &ProviderMappingEntry, entries: &mut Vec<ValidationEntry>) {
        // Route registration resolution
        let route = match self.routes.get(&entry.route_registration) {
            Some(r) => r,
            None => {
                entries.push(ValidationEntry {
                    code: "missing_route_registration".to_string(),
                    severity: ValidationSeverity::Error,
                    message: format!(
                        "route_registration '{}' does not resolve to any Holon provider definition",
                        entry.route_registration
                    ),
                    entry_identity: Some(entry.models_dev_id.clone()),
                    source_value: Some(entry.route_registration.clone()),
                    effective_value: None,
                });
                return;
            }
        };

        // Transport match
        let expected = transport_wire_name(route.transport);
        if entry.transport != expected {
            entries.push(ValidationEntry {
                code: "transport_mismatch".to_string(),
                severity: ValidationSeverity::Error,
                message: format!(
                    "transport '{}' does not match route registration transport '{}'",
                    entry.transport, expected
                ),
                entry_identity: Some(entry.models_dev_id.clone()),
                source_value: Some(entry.transport.clone()),
                effective_value: Some(expected.to_string()),
            });
        }

        // Kind/transport compatibility
        let kind_ok = match entry.kind {
            ProviderKind::Direct => entry.transport == expected,
            ProviderKind::OpenAiCompatible => matches!(
                entry.transport.as_str(),
                "openai_chat_completions" | "openai_responses" | "openai_codex_responses"
            ),
            ProviderKind::Gateway | ProviderKind::TokenHub => true,
        };
        if !kind_ok {
            entries.push(ValidationEntry {
                code: "kind_transport_mismatch".to_string(),
                severity: ValidationSeverity::Error,
                message: format!(
                    "provider kind '{:?}' is incompatible with transport '{}'",
                    entry.kind, entry.transport
                ),
                entry_identity: Some(entry.models_dev_id.clone()),
                source_value: Some(format!("{:?}", entry.kind)),
                effective_value: Some(entry.transport.clone()),
            });
        }

        // Credential reference check
        let cred_ok = entry.credential_ref == route.legacy_provider
            || route
                .credential_envs
                .iter()
                .any(|env| env == &entry.credential_ref);
        if !cred_ok {
            entries.push(ValidationEntry {
                code: "unknown_credential_ref".to_string(),
                severity: ValidationSeverity::Error,
                message: format!(
                    "credential_ref '{}' is not a declared configuration slot for route '{}'",
                    entry.credential_ref, entry.route_registration
                ),
                entry_identity: Some(entry.models_dev_id.clone()),
                source_value: Some(entry.credential_ref.clone()),
                effective_value: Some(format!(
                    "provider={} envs={}",
                    route.legacy_provider,
                    route.credential_envs.join(",")
                )),
            });
        }

        // Provenance completeness
        if entry.provenance.reviewed_at.is_none() {
            entries.push(ValidationEntry {
                code: "incomplete_provenance".to_string(),
                severity: ValidationSeverity::Warning,
                message: format!(
                    "mapping for '{}' has no reviewed_at date",
                    entry.models_dev_id
                ),
                entry_identity: Some(entry.models_dev_id.clone()),
                source_value: None,
                effective_value: None,
            });
        }

        // Disabled mapping info
        if !entry.enabled {
            entries.push(ValidationEntry {
                code: "mapping_disabled".to_string(),
                severity: ValidationSeverity::Info,
                message: format!(
                    "mapping for '{}' is disabled (enabled=false)",
                    entry.models_dev_id
                ),
                entry_identity: Some(entry.models_dev_id.clone()),
                source_value: Some("false".to_string()),
                effective_value: Some("discovery_only".to_string()),
            });
        }
    }

    fn validate_against_snapshot(
        &self,
        manifest: &ProviderMappingManifest,
        snapshot: &ModelsDevSnapshot,
        entries: &mut Vec<ValidationEntry>,
        unmapped: &mut Vec<String>,
        offerings: &mut Vec<OfferingRecord>,
    ) {
        let mapping: BTreeMap<&str, &ProviderMappingEntry> = manifest
            .providers
            .iter()
            .map(|p| (p.models_dev_id.as_str(), p))
            .collect();

        for (md_provider_id, provider) in &snapshot.providers {
            match mapping.get(md_provider_id.as_str()) {
                Some(entry) => {
                    for (md_model_id, model) in &provider.models {
                        let allowed = is_model_allowed(&entry.model_id, md_model_id);
                        if !allowed {
                            entries.push(ValidationEntry {
                                code: "model_outside_allowlist".to_string(),
                                severity: ValidationSeverity::Warning,
                                message: format!(
                                    "model '{}/{}' is outside the allowlist",
                                    md_provider_id, md_model_id
                                ),
                                entry_identity: Some(format!("{}/{}", md_provider_id, md_model_id)),
                                source_value: Some(md_model_id.clone()),
                                effective_value: Some("excluded".to_string()),
                            });
                        } else {
                            // Check capability ceiling
                            self.check_capability_ceiling(
                                entry,
                                md_provider_id,
                                md_model_id,
                                model,
                                entries,
                            );
                            // Generate offering record (discovery_only)
                            offerings.push(OfferingRecord {
                                models_dev_ref: format!("{}/{}", md_provider_id, md_model_id),
                                holon_provider_id: entry.holon_provider_id.clone(),
                                model_identity: md_model_id.clone(),
                                offering_id: format!("{}/{}", md_provider_id, md_model_id),
                                route_registration: entry.route_registration.clone(),
                                callability: Callability::DiscoveryOnly,
                            });
                        }
                    }
                }
                None => {
                    unmapped.push(md_provider_id.clone());
                }
            }
        }
    }

    fn check_capability_ceiling(
        &self,
        entry: &ProviderMappingEntry,
        provider_id: &str,
        model_id: &str,
        model: &super::dto::ModelsDevModel,
        entries: &mut Vec<ValidationEntry>,
    ) {
        let identity = format!("{}/{}", provider_id, model_id);

        // Check modalities against capability ceiling
        if let Some(modalities) = &model.modalities {
            if modalities.input.iter().any(|m| m == "image")
                && !entry.capability_ceiling.image_input
            {
                entries.push(ValidationEntry {
                    code: "capability_widening_image_input".to_string(),
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "upstream claims image_input for '{}' but ceiling is false",
                        identity
                    ),
                    entry_identity: Some(identity.clone()),
                    source_value: Some("image_input=true".to_string()),
                    effective_value: Some("false".to_string()),
                });
            }
            if modalities.output.iter().any(|m| m == "image")
                && !entry.capability_ceiling.image_generation
            {
                entries.push(ValidationEntry {
                    code: "capability_widening_image_generation".to_string(),
                    severity: ValidationSeverity::Warning,
                    message: format!(
                        "upstream claims image_generation for '{}' but ceiling is false",
                        identity
                    ),
                    entry_identity: Some(identity.clone()),
                    source_value: Some("image_output=true".to_string()),
                    effective_value: Some("false".to_string()),
                });
            }
        }

        // Check tool_call against capability ceiling
        if model.tool_call == Some(true) && !entry.capability_ceiling.tool_calling {
            entries.push(ValidationEntry {
                code: "capability_widening_tool_calling".to_string(),
                severity: ValidationSeverity::Warning,
                message: format!(
                    "upstream claims tool_calling for '{}' but ceiling is false",
                    identity
                ),
                entry_identity: Some(identity.clone()),
                source_value: Some("tool_call=true".to_string()),
                effective_value: Some("false".to_string()),
            });
        }

        // Check reasoning against capability ceiling
        if model.reasoning == Some(true) && !entry.capability_ceiling.reasoning {
            entries.push(ValidationEntry {
                code: "capability_widening_reasoning".to_string(),
                severity: ValidationSeverity::Warning,
                message: format!(
                    "upstream claims reasoning for '{}' but ceiling is false",
                    identity
                ),
                entry_identity: Some(identity.clone()),
                source_value: Some("reasoning=true".to_string()),
                effective_value: Some("false".to_string()),
            });
        }

        // Check structured_output against capability ceiling
        if model.structured_output == Some(true) && !entry.capability_ceiling.structured_output {
            entries.push(ValidationEntry {
                code: "capability_widening_structured_output".to_string(),
                severity: ValidationSeverity::Warning,
                message: format!(
                    "upstream claims structured_output for '{}' but ceiling is false",
                    identity
                ),
                entry_identity: Some(identity.clone()),
                source_value: Some("structured_output=true".to_string()),
                effective_value: Some("false".to_string()),
            });
        }

        // Check limits against limit ceiling
        if let Some(limit) = &model.limit {
            if let Some(ctx) = limit.context {
                if let Some(ceiling) = entry.limit_ceiling.context_window_tokens {
                    if ctx > ceiling {
                        entries.push(ValidationEntry {
                            code: "limit_exceeds_ceiling_context".to_string(),
                            severity: ValidationSeverity::Warning,
                            message: format!(
                                "upstream context_window_tokens {} exceeds ceiling {} for '{}'",
                                ctx, ceiling, identity
                            ),
                            entry_identity: Some(identity.clone()),
                            source_value: Some(ctx.to_string()),
                            effective_value: Some(ceiling.to_string()),
                        });
                    }
                }
            }
            if let Some(output) = limit.output {
                if let Some(ceiling) = entry.limit_ceiling.max_output_tokens {
                    if output > ceiling {
                        entries.push(ValidationEntry {
                            code: "limit_exceeds_ceiling_output".to_string(),
                            severity: ValidationSeverity::Warning,
                            message: format!(
                                "upstream max_output_tokens {} exceeds ceiling {} for '{}'",
                                output, ceiling, identity
                            ),
                            entry_identity: Some(identity.clone()),
                            source_value: Some(output.to_string()),
                            effective_value: Some(ceiling.to_string()),
                        });
                    }
                }
            }
        }
    }
}

impl Default for ValidationEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::super::mapping::{
        CapabilityCeiling, LimitCeiling, MappingProvenance, ModelIdAllow, ModelIdMatchMode,
    };
    use super::*;
    use crate::model_catalog::models_dev::dto::{
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
        let raw = include_str!("manifests/anthropic_v1.json");
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
            .any(|e| e.code == "model_outside_allowlist"
                && e.severity == ValidationSeverity::Warning));
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
        assert!(report.entries.iter().any(
            |e| e.code == "incomplete_provenance" && e.severity == ValidationSeverity::Warning
        ));
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
}
