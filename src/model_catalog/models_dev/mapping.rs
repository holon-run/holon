//! Provider mapping manifest schema for models.dev integration (Phase 3A).
//!
//! The manifest is a versioned, Holon-owned allowlist that connects
//! `models.dev` upstream provider IDs to Holon provider/route identities.
//! It is not an endpoint discovery mechanism: it cannot create base URLs,
//! credentials, transports, or routes. It only declares relationships that
//! the validation engine checks against existing Holon registrations.
//!
//! See `docs/rfcs/models-dev-provider-mapping.md` for the normative RFC.

use serde::{Deserialize, Serialize};

/// Supported mapping manifest schema version.
pub const MAPPING_SCHEMA_VERSION: u32 = 1;

/// Provider kind classification for a mapping entry.
///
/// Distinguishes direct providers, OpenAI-compatible providers, gateways,
/// and token hubs to prevent conflating transport protocols.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Direct provider using its native protocol (e.g. Anthropic Messages).
    Direct,
    /// Provider using an OpenAI-compatible wire protocol.
    OpenAiCompatible,
    /// Gateway or aggregator that proxies multiple upstream providers.
    Gateway,
    /// Special plan or token hub with non-standard billing/access.
    TokenHub,
}

/// Model ID matching mode for the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelIdMatchMode {
    /// Only exact string matches are allowed.
    Exact,
    /// Exact matches and glob-style patterns (e.g. `claude-*`) are allowed.
    ExactOrPattern,
    /// Only glob-style patterns are interpreted; bare IDs must be patterns.
    Pattern,
}

impl Default for ModelIdMatchMode {
    fn default() -> Self {
        Self::ExactOrPattern
    }
}

/// Model ID allowlist configuration.
///
/// `allow` is an explicit list of exact IDs or glob patterns. A mapping
/// cannot authorize model IDs outside this set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelIdAllow {
    #[serde(default)]
    pub mode: ModelIdMatchMode,
    pub allow: Vec<String>,
}

/// Capability ceiling: upper bounds for model capabilities.
///
/// These are maximum allowed values; the effective capability is the
/// intersection of upstream assertion, manifest ceiling, route registration
/// support, and runtime safety policy.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityCeiling {
    #[serde(default)]
    pub tool_calling: bool,
    #[serde(default)]
    pub image_input: bool,
    #[serde(default)]
    pub image_generation: bool,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub structured_output: bool,
}

/// Limit ceiling: upper bounds for model limits.
///
/// Effective limits are the most restrictive applicable bound.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitCeiling {
    #[serde(default)]
    pub context_window_tokens: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
}

/// Provenance for a mapping manifest entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MappingProvenance {
    #[serde(default = "default_owner")]
    pub owner: String,
    #[serde(default)]
    pub reviewed_at: Option<String>,
}

fn default_owner() -> String {
    "holon".to_string()
}

impl Default for MappingProvenance {
    fn default() -> Self {
        Self {
            owner: default_owner(),
            reviewed_at: None,
        }
    }
}

/// A single provider mapping entry in the manifest.
///
/// Maps one `models.dev` provider ID to a Holon provider/route identity.
/// `route_registration` must resolve to an existing Holon provider
/// definition; the manifest cannot create endpoints, credentials, or
/// transports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMappingEntry {
    /// Exact upstream `models.dev` provider ID. No fuzzy matching.
    pub models_dev_id: String,
    /// Holon provider identity (e.g. `anthropic`).
    pub holon_provider_id: String,
    /// Provider kind: direct, openai_compatible, gateway, or token_hub.
    pub kind: ProviderKind,
    /// Transport wire name (e.g. `anthropic_messages`).
    pub transport: String,
    /// Route registration key: `provider@endpoint` (e.g. `anthropic@default`).
    pub route_registration: String,
    /// Explicit model ID allowlist/pattern set.
    pub model_id: ModelIdAllow,
    /// Upper bounds for capabilities.
    #[serde(default)]
    pub capability_ceiling: CapabilityCeiling,
    /// Upper bounds for limits.
    #[serde(default)]
    pub limit_ceiling: LimitCeiling,
    /// Names an existing configuration slot only; must not read or create secrets.
    pub credential_ref: String,
    /// Whether the mapping is enabled. Defaults to `false` in Phase 3A.
    #[serde(default)]
    pub enabled: bool,
    /// Review provenance.
    #[serde(default)]
    pub provenance: MappingProvenance,
}

/// The top-level provider mapping manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderMappingManifest {
    /// Manifest schema version. Must equal [`MAPPING_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Provider mapping entries.
    pub providers: Vec<ProviderMappingEntry>,
}

impl ProviderMappingManifest {
    /// Parses a manifest from raw JSON.
    pub fn parse(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    /// Finds a provider mapping entry by `models.dev` ID.
    pub fn find_provider(&self, models_dev_id: &str) -> Option<&ProviderMappingEntry> {
        self.providers
            .iter()
            .find(|p| p.models_dev_id == models_dev_id)
    }
}

/// Callability of an offering record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Callability {
    /// Validated but not callable; appears in discovery/report only.
    DiscoveryOnly,
    /// Callable via a registered route (requires `enabled = true`).
    Callable,
}

impl Default for Callability {
    fn default() -> Self {
        Self::DiscoveryOnly
    }
}

/// An offering record connecting models.dev data to Holon identities.
///
/// Generated by the validation engine for each validated offering. The
/// report must not synthesize a route when `route_registration` is absent,
/// unknown, disabled, or incompatible with the mapped transport.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OfferingRecord {
    /// Upstream reference: `provider/model_id`.
    pub models_dev_ref: String,
    /// Holon provider identity.
    pub holon_provider_id: String,
    /// Canonical model identity.
    pub model_identity: String,
    /// Offering identity: `provider/model_id`.
    pub offering_id: String,
    /// Route registration key.
    pub route_registration: String,
    /// Callability result (always `discovery_only` in Phase 3A).
    pub callability: Callability,
}

/// Checks whether a model ID matches the allowlist.
///
/// In `Exact` mode, only exact string matches pass. In `ExactOrPattern`
/// and `Pattern` modes, glob-style patterns (`*` wildcard) are supported.
pub fn is_model_allowed(allow: &ModelIdAllow, model_id: &str) -> bool {
    match allow.mode {
        ModelIdMatchMode::Exact => allow.allow.iter().any(|p| p == model_id),
        ModelIdMatchMode::ExactOrPattern | ModelIdMatchMode::Pattern => {
            allow.allow.iter().any(|p| glob_match(p, model_id))
        }
    }
}

/// Simple glob matcher with `*` wildcard support.
fn glob_match(pattern: &str, value: &str) -> bool {
    if !pattern.contains('*') {
        return pattern == value;
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 2 {
        let (prefix, suffix) = (parts[0], parts[1]);
        value.starts_with(prefix)
            && value.ends_with(suffix)
            && value.len() >= prefix.len() + suffix.len()
    } else {
        // Multiple wildcards: fall back to segment-by-segment matching
        let mut rest = value;
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() {
                continue;
            }
            if i == 0 {
                if !rest.starts_with(part) {
                    return false;
                }
                rest = &rest[part.len()..];
            } else if let Some(pos) = rest.find(part) {
                rest = &rest[pos + part.len()..];
            } else {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_anthropic_manifest() {
        let raw = include_str!("manifests/anthropic_v1.json");
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
}
