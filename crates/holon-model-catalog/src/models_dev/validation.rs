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

use holon_domain::model_values::provider_definitions;
use holon_domain::model_values::ProviderTransportKind;

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
        ProviderTransportKind::AiEvaluationModel => "ai_evaluation_model",
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
        // Register the derived `deepseek@responses` route. This endpoint is
        // synthesized at runtime from `deepseek@default` (see config::models),
        // using OpenAiResponses transport with the same DEEPSEEK_API_KEY
        // credential. It is not a standalone ProviderDefinition entry because
        // its base URL and web-search config are derived from the default
        // endpoint at runtime, but it is a valid Holon route identity.
        if let Some(default) = routes.get("deepseek@default") {
            routes.insert(
                "deepseek@responses".to_string(),
                RouteRegistration {
                    transport: ProviderTransportKind::OpenAiResponses,
                    credential_envs: default.credential_envs.clone(),
                    legacy_provider: default.legacy_provider.clone(),
                },
            );
        }
        Self { routes }
    }

    pub(super) fn contains_route(&self, route_registration: &str) -> bool {
        self.routes.contains_key(route_registration)
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
mod tests;
