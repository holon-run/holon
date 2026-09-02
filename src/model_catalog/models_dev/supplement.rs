//! Supplemental catalog drafting from `models.dev`.
//!
//! The supplement turns the weekly refresh from a passive radar into a
//! review-gated supply line for already-supported providers:
//!
//! - Only providers on [`AUTO_SUPPLEMENT_PROVIDERS`] get auto-drafted.
//!   Aggregators and gateways stay out; their offerings remain audit data.
//! - Only models missing from the compiled-in legacy catalog are drafted;
//!   existing Holon entries are never overridden by upstream metadata.
//! - A model must emit `text` output and carry a `release_date` (or
//!   `last_updated`) inside [`RECENCY_WINDOW_DAYS`] to be drafted. The
//!   window gates entry only: admitted entries are retained while they
//!   remain in the upstream snapshot, so the catalog does not flap.
//! - Generation is incremental: the previous checked-in supplement is the
//!   retention baseline; entries disappear only when the model vanishes
//!   upstream or is promoted into the legacy catalog.
//!
//! The runtime merges the checked-in supplement into the built-in catalog
//! (see `model_catalog::snapshot`). Nothing becomes callable without PR
//! review and merge; the supplement never changes routes, credentials, or
//! provider enablement.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::config::{ModelRef, ModelRouteRef, ProviderEndpointId};
use crate::model_catalog::{
    BuiltInModelCatalog, BuiltInModelMetadata, BuiltInModelRoutePolicy, ModelMetadataSource,
};

use super::dto::{ModelsDevModel, ModelsDevSnapshot};
use super::projection::{project_model, ProviderMapping};

pub const SUPPLEMENT_SCHEMA_VERSION: u32 = 1;

/// models.dev provider IDs whose new models are auto-drafted into the
/// supplemental catalog. Curated first-party and primary-hosted providers
/// only; aggregators (openrouter, huggingface, nvidia, venice, nearai,
/// together, fireworks, chutes, arcee) deliberately stay out because their
/// catalogs mirror other providers and would flood review with duplicates.
pub const AUTO_SUPPLEMENT_PROVIDERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "xai",
    "deepseek",
    "moonshotai",
    "zhipuai",
    "zai",
    "minimax",
    "alibaba",
    "xiaomi",
    "stepfun",
    "volcengine",
    "mistral",
];

/// How many days back a model's upstream release date may lie and still be
/// auto-drafted. Gates entry only; retention is sticky.
pub const RECENCY_WINDOW_DAYS: i64 = 120;

/// The checked-in supplemental catalog DTO.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ModelsDevSupplement {
    pub schema_version: u32,
    /// SHA-256 of the `models.dev/api.json` payload this supplement was
    /// generated from.
    #[serde(default)]
    pub upstream_revision: String,
    /// Holon crate version of the generator.
    #[serde(default)]
    pub adapter_version: String,
    pub models: Vec<BuiltInModelMetadata>,
}

impl ModelsDevSupplement {
    /// The bootstrap supplement: valid, empty, no provenance.
    pub fn empty() -> Self {
        Self {
            schema_version: SUPPLEMENT_SCHEMA_VERSION,
            upstream_revision: String::new(),
            adapter_version: String::new(),
            models: Vec::new(),
        }
    }

    /// Parses and structurally validates a supplement JSON document.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let supplement = serde_json::from_str::<ModelsDevSupplement>(raw)
            .map_err(|error| format!("failed to parse models.dev supplement: {error}"))?;
        supplement.validate()?;
        Ok(supplement)
    }

    /// Serializes deterministically (models sorted by model ref).
    pub fn to_json(&self) -> Result<String, String> {
        let mut sorted = self.clone();
        sorted
            .models
            .sort_by_key(|model| model.model_ref.as_string());
        serde_json::to_string_pretty(&sorted)
            .map_err(|error| format!("failed to serialize models.dev supplement: {error}"))
    }

    /// Structural validation independent of the legacy catalog.
    pub fn validate(&self) -> Result<(), String> {
        for model in &self.models {
            crate::model_catalog::snapshot::validate_model_entry(model)?;
        }
        if self.schema_version != SUPPLEMENT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported supplement schema version {}; expected {SUPPLEMENT_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.models.is_empty() {
            return Ok(());
        }
        if self.upstream_revision.trim().is_empty() {
            return Err("non-empty supplement must record upstream_revision".to_string());
        }
        if self.adapter_version.trim().is_empty() {
            return Err("non-empty supplement must record adapter_version".to_string());
        }
        let mut seen = HashSet::new();
        for model in &self.models {
            if model.source != ModelMetadataSource::ModelsDevSupplement {
                return Err(format!(
                    "supplement model {} must declare source models_dev_supplement",
                    model.model_ref.as_string()
                ));
            }
            if model.endpoint.is_some() {
                return Err(format!(
                    "supplement model {} must not declare an endpoint",
                    model.model_ref.as_string()
                ));
            }
            if !seen.insert(model.model_ref.clone()) {
                return Err(format!(
                    "duplicate supplement model {}",
                    model.model_ref.as_string()
                ));
            }
        }
        Ok(())
    }

    /// Validates this supplement against the compiled-in legacy catalog and
    /// merges its entries (plus default-endpoint routes) into `catalog`.
    pub fn apply_to(&self, catalog: &mut BuiltInModelCatalog) -> Result<(), String> {
        self.validate()?;
        for model in &self.models {
            if catalog.entries.contains_key(&model.model_ref) {
                return Err(format!(
                    "supplement model {} collides with the built-in catalog",
                    model.model_ref.as_string()
                ));
            }
            if catalog.aliases.contains_key(&model.model_ref) {
                return Err(format!(
                    "supplement model {} collides with a built-in alias",
                    model.model_ref.as_string()
                ));
            }
        }
        for model in &self.models {
            let route_ref = ModelRouteRef::new(
                model.model_ref.provider.clone(),
                ProviderEndpointId::default_endpoint(),
                model.model_ref.model.clone(),
            );
            catalog
                .entries
                .insert(model.model_ref.clone(), model.clone());
            catalog
                .route_entries
                .entry(route_ref)
                .or_insert_with(BuiltInModelRoutePolicy::default);
        }
        Ok(())
    }
}

/// Why a candidate model was not auto-drafted.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeferredReason {
    /// Output modalities do not include text (image/audio/embedding-only).
    NotTextOutput,
    /// Release date is older than the recency window.
    ReleaseOutsideWindow,
    /// No usable release date upstream; needs a human decision.
    NoReleaseDate,
    /// The model ref is a legacy alias key and cannot be shadowed.
    LegacyAliasConflict,
    /// The provider/model id pair cannot form a valid model ref.
    InvalidModelRef,
}

/// Why a previous supplement entry disappeared from the new supplement.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemovalReason {
    /// The model was promoted into the compiled-in legacy catalog.
    PromotedToLegacy,
    /// The model vanished from the upstream snapshot.
    MissingUpstream,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredModel {
    pub model_ref: String,
    pub reason: DeferredReason,
    pub release_date: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemovedModel {
    pub model_ref: String,
    pub reason: RemovalReason,
}

/// The outcome of supplement generation.
#[derive(Debug, Clone, PartialEq)]
pub struct SupplementUpdate {
    /// The complete regenerated supplement to check in.
    pub supplement: ModelsDevSupplement,
    /// Models drafted in this run (new since the previous supplement).
    pub drafted: Vec<BuiltInModelMetadata>,
    /// Model refs retained from the previous supplement.
    pub retained: Vec<String>,
    /// Model refs dropped from the previous supplement, with reasons.
    pub removed: Vec<RemovedModel>,
    /// Allowlisted candidates that were not drafted, with reasons.
    pub deferred: Vec<DeferredModel>,
    /// Mapped models.dev providers that are not on the supplement allowlist.
    pub providers_not_allowlisted: Vec<String>,
}

/// Generates the next supplement from a snapshot and the previous state.
///
/// `cutoff` is the earliest `YYYY-MM-DD` release date still inside the
/// recency window (ISO dates compare correctly as strings).
pub fn generate(
    snapshot: &ModelsDevSnapshot,
    previous: Option<&ModelsDevSupplement>,
    legacy: &BuiltInModelCatalog,
    cutoff: &str,
    upstream_revision: &str,
    adapter_version: &str,
) -> Result<SupplementUpdate, String> {
    let default_mappings = ProviderMapping::default_mappings();
    let mappings: BTreeMap<&str, &ProviderMapping> = default_mappings
        .iter()
        .map(|mapping| (mapping.models_dev_id.as_str(), mapping))
        .collect();
    let allowlist: BTreeSet<&str> = AUTO_SUPPLEMENT_PROVIDERS.iter().copied().collect();

    let previous_entries: BTreeMap<String, &BuiltInModelMetadata> = previous
        .map(|supplement| {
            supplement
                .models
                .iter()
                .map(|model| (model.model_ref.as_string(), model))
                .collect()
        })
        .unwrap_or_default();
    let mut remaining_previous: BTreeSet<String> = previous_entries.keys().cloned().collect();

    let mut drafted: Vec<BuiltInModelMetadata> = Vec::new();
    let mut retained: Vec<String> = Vec::new();
    let mut deferred: Vec<DeferredModel> = Vec::new();
    let mut promoted: Vec<String> = Vec::new();
    let mut providers_not_allowlisted: Vec<String> = Vec::new();

    for (md_provider_id, provider) in &snapshot.providers {
        let Some(mapping) = mappings.get(md_provider_id.as_str()) else {
            continue;
        };
        if !allowlist.contains(md_provider_id.as_str()) {
            providers_not_allowlisted.push(md_provider_id.clone());
            continue;
        }
        let holon_provider_id = mapping.holon_provider_id.clone();
        for (md_model_id, model) in &provider.models {
            let Ok(model_ref) =
                ModelRef::parse(format!("{holon_provider_id}/{md_model_id}").as_str())
            else {
                deferred.push(DeferredModel {
                    model_ref: format!("{holon_provider_id}/{md_model_id}"),
                    reason: DeferredReason::InvalidModelRef,
                    release_date: None,
                });
                continue;
            };
            let model_ref_string = model_ref.as_string();
            if legacy.entries.contains_key(&model_ref) {
                if remaining_previous.remove(&model_ref_string) {
                    promoted.push(model_ref_string);
                }
                continue;
            }
            if legacy.aliases.contains_key(&model_ref) {
                deferred.push(DeferredModel {
                    model_ref: model_ref_string,
                    reason: DeferredReason::LegacyAliasConflict,
                    release_date: release_date_key(model).map(str::to_string),
                });
                continue;
            }
            if remaining_previous.remove(&model_ref_string) {
                // Retained: refresh metadata from the current snapshot but
                // keep the entry admitted.
                retained.push(model_ref_string);
                continue;
            }
            let Some(modalities) = model.modalities.as_ref() else {
                deferred.push(DeferredModel {
                    model_ref: model_ref_string,
                    reason: DeferredReason::NotTextOutput,
                    release_date: release_date_key(model).map(str::to_string),
                });
                continue;
            };
            if !modalities.output.iter().any(|m| m == "text") {
                deferred.push(DeferredModel {
                    model_ref: model_ref_string,
                    reason: DeferredReason::NotTextOutput,
                    release_date: release_date_key(model).map(str::to_string),
                });
                continue;
            }
            let Some(date_key) = release_date_key(model) else {
                deferred.push(DeferredModel {
                    model_ref: model_ref_string,
                    reason: DeferredReason::NoReleaseDate,
                    release_date: None,
                });
                continue;
            };
            if date_key < cutoff {
                deferred.push(DeferredModel {
                    model_ref: model_ref_string,
                    reason: DeferredReason::ReleaseOutsideWindow,
                    release_date: Some(date_key.to_string()),
                });
                continue;
            }
            let mut metadata = project_model(&model_ref, model);
            metadata.source = ModelMetadataSource::ModelsDevSupplement;
            metadata.endpoint = None;
            drafted.push(metadata);
        }
    }

    // Retained entries need re-projection from the current snapshot; collect
    // them from the snapshot loop above is awkward, so rebuild here from the
    // retained ref list.
    let mut models: Vec<BuiltInModelMetadata> = drafted.clone();
    for model_ref_string in &retained {
        let model_ref = ModelRef::parse(model_ref_string)
            .map_err(|error| format!("invalid retained model ref {model_ref_string}: {error}"))?;
        let md_provider_id = mappings
            .iter()
            .find(|(_, mapping)| mapping.holon_provider_id == model_ref.provider.as_str())
            .map(|(md_id, _)| md_id.to_string())
            .ok_or_else(|| format!("retained model {model_ref_string} has no provider mapping"))?;
        let provider = snapshot.providers.get(&md_provider_id).ok_or_else(|| {
            format!("retained model {model_ref_string} lost its upstream provider")
        })?;
        let model = provider
            .models
            .get(model_ref.model.as_str())
            .ok_or_else(|| format!("retained model {model_ref_string} lost its upstream entry"))?;
        let mut metadata = project_model(&model_ref, model);
        metadata.source = ModelMetadataSource::ModelsDevSupplement;
        metadata.endpoint = None;
        models.push(metadata);
    }

    let mut removed = remaining_previous
        .into_iter()
        .map(|model_ref_string| {
            let is_promoted = ModelRef::parse(&model_ref_string)
                .map(|model_ref| legacy.entries.contains_key(&model_ref))
                .unwrap_or(false);
            RemovedModel {
                reason: if is_promoted {
                    RemovalReason::PromotedToLegacy
                } else {
                    RemovalReason::MissingUpstream
                },
                model_ref: model_ref_string,
            }
        })
        .collect::<Vec<_>>();
    removed.extend(promoted.into_iter().map(|model_ref| RemovedModel {
        model_ref,
        reason: RemovalReason::PromotedToLegacy,
    }));

    models.sort_by_key(|model| model.model_ref.as_string());
    retained.sort();
    deferred.sort_by(|a, b| a.model_ref.cmp(&b.model_ref));
    removed.sort_by(|a, b| a.model_ref.cmp(&b.model_ref));
    providers_not_allowlisted.sort();

    Ok(SupplementUpdate {
        supplement: ModelsDevSupplement {
            schema_version: SUPPLEMENT_SCHEMA_VERSION,
            upstream_revision: upstream_revision.to_string(),
            adapter_version: adapter_version.to_string(),
            models,
        },
        drafted,
        retained,
        removed,
        deferred,
        providers_not_allowlisted,
    })
}

fn release_date_key(model: &ModelsDevModel) -> Option<&str> {
    model
        .release_date
        .as_deref()
        .or(model.last_updated.as_deref())
        .filter(|value| !value.trim().is_empty())
}

/// Renders the refresh PR summary markdown for a supplement update.
pub fn render_summary_markdown(update: &SupplementUpdate) -> String {
    const LIST_CAP: usize = 40;
    let mut out = String::new();
    out.push_str("# models.dev refresh summary\n\n");
    out.push_str(&format!(
        "- Supplement models: {} (drafted this run: {}, retained: {}, removed: {})\n",
        update.supplement.models.len(),
        update.drafted.len(),
        update.retained.len(),
        update.removed.len()
    ));
    out.push_str(&format!(
        "- Deferred candidates: {} (not auto-drafted; see below)\n",
        update.deferred.len()
    ));
    out.push_str(&format!(
        "- Mapped providers without auto-supplement (aggregators etc.): {}\n",
        update.providers_not_allowlisted.len()
    ));

    out.push_str("\n## Auto-drafted supplement models\n\n");
    if update.drafted.is_empty() {
        out.push_str("(none this run)\n");
    } else {
        for model in &update.drafted {
            out.push_str(&format!(
                "- `{}` — {} (context {}, reasoning {}, image input {})\n",
                model.model_ref.as_string(),
                model.display_name,
                model
                    .context_window_tokens
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "unknown".into()),
                model.capabilities.supports_reasoning,
                model.capabilities.image_input
            ));
        }
    }

    if !update.removed.is_empty() {
        out.push_str("\n## Removed from previous supplement\n\n");
        for removed in update.removed.iter().take(LIST_CAP) {
            out.push_str(&format!(
                "- `{}` — {:?}\n",
                removed.model_ref, removed.reason
            ));
        }
        if update.removed.len() > LIST_CAP {
            out.push_str(&format!(
                "- … and {} more\n",
                update.removed.len() - LIST_CAP
            ));
        }
    }

    out.push_str("\n## Deferred (needs human decision or outside policy)\n\n");
    if update.deferred.is_empty() {
        out.push_str("(none)\n");
    } else {
        for deferred in update.deferred.iter().take(LIST_CAP) {
            out.push_str(&format!(
                "- `{}` — {:?} (release {})\n",
                deferred.model_ref,
                deferred.reason,
                deferred.release_date.as_deref().unwrap_or("unknown")
            ));
        }
        if update.deferred.len() > LIST_CAP {
            out.push_str(&format!(
                "- … and {} more (see `holon models-dev audit`)\n",
                update.deferred.len() - LIST_CAP
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_catalog::snapshot;

    fn snapshot_json() -> String {
        r#"{
            "mistral": {
                "id": "mistral",
                "models": {
                    "magistral-small": {"id": "magistral-small", "release_date": "2026-08-30",
                        "modalities": {"input": ["text"], "output": ["text"]}}
                }
            },
            "xai": {
                "id": "xai",
                "models": {
                    "grok-4.3": {"id": "grok-4.3", "name": "Grok 4.3", "release_date": "2026-01-10",
                        "modalities": {"input": ["text"], "output": ["text"]}},
                    "grok-new-9": {"id": "grok-new-9", "name": "Grok New 9", "release_date": "2026-08-30",
                        "modalities": {"input": ["text", "image"], "output": ["text"]},
                        "limit": {"context": 400000, "output": 64000}, "reasoning": true},
                    "grok-old-1": {"id": "grok-old-1", "name": "Grok Old 1", "release_date": "2024-01-01",
                        "modalities": {"input": ["text"], "output": ["text"]}},
                    "grok-nodate": {"id": "grok-nodate", "name": "Grok No Date",
                        "modalities": {"input": ["text"], "output": ["text"]}},
                    "grok-image": {"id": "grok-image", "release_date": "2026-08-30",
                        "modalities": {"input": ["text"], "output": ["image"]}},
                    "grok-retained": {"id": "grok-retained", "name": "Grok Retained", "release_date": "2026-01-05",
                        "modalities": {"input": ["text"], "output": ["text"]}}
                }
            },
            "openrouter": {
                "id": "openrouter",
                "models": {
                    "some/model": {"id": "some/model", "release_date": "2026-08-30",
                        "modalities": {"input": ["text"], "output": ["text"]}}
                }
            }
        }"#
        .to_string()
    }

    fn legacy() -> BuiltInModelCatalog {
        snapshot::legacy_catalog().expect("legacy catalog must parse")
    }

    fn supplement_metadata(model_ref: &str, display_name: &str) -> BuiltInModelMetadata {
        BuiltInModelMetadata {
            model_ref: ModelRef::parse(model_ref).unwrap(),
            display_name: display_name.to_string(),
            description: "supplement test entry".to_string(),
            context_window_tokens: None,
            effective_context_window_percent: 95,
            auto_compact_token_limit: None,
            default_max_output_tokens: None,
            max_output_tokens_upper_limit: None,
            default_verbosity: None,
            tool_output_truncation_estimated_tokens: Some(2_500),
            capabilities: Default::default(),
            reasoning_effort_options: Vec::new(),
            source: ModelMetadataSource::ModelsDevSupplement,
            endpoint: None,
        }
    }

    fn previous() -> ModelsDevSupplement {
        let metadata = supplement_metadata("xai/grok-retained", "Grok Retained");
        let vanished = supplement_metadata("xai/grok-vanished", "Grok Vanished");
        ModelsDevSupplement {
            schema_version: SUPPLEMENT_SCHEMA_VERSION,
            upstream_revision: "abc".into(),
            adapter_version: "test".into(),
            models: vec![metadata, vanished],
        }
    }

    #[test]
    fn drafts_only_recent_text_models_of_allowlisted_providers() {
        let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
        let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();

        let drafted_refs: Vec<String> = update
            .drafted
            .iter()
            .map(|model| model.model_ref.as_string())
            .collect();
        assert_eq!(drafted_refs, vec!["xai/grok-new-9".to_string()]);
        let drafted_model = &update.drafted[0];
        assert_eq!(
            drafted_model.source,
            ModelMetadataSource::ModelsDevSupplement
        );
        assert_eq!(drafted_model.context_window_tokens, Some(400_000));
        assert!(drafted_model.capabilities.supports_reasoning);
        assert!(drafted_model.capabilities.image_input);

        let deferred: Vec<(&str, DeferredReason)> = update
            .deferred
            .iter()
            .map(|d| (d.model_ref.as_str(), d.reason))
            .collect();
        assert!(deferred.contains(&("xai/grok-old-1", DeferredReason::ReleaseOutsideWindow)));
        assert!(deferred.contains(&("xai/grok-nodate", DeferredReason::NoReleaseDate)));
        assert!(deferred.contains(&("xai/grok-image", DeferredReason::NotTextOutput)));
        assert_eq!(
            update.providers_not_allowlisted,
            vec!["openrouter".to_string()]
        );
    }

    #[test]
    fn retains_sticky_entries_and_removes_missing_ones() {
        let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
        let prev = previous();
        let update = generate(&parsed, Some(&prev), &legacy(), "2026-05-05", "rev", "ver").unwrap();

        assert_eq!(update.retained, vec!["xai/grok-retained".to_string()]);
        assert_eq!(update.drafted.len(), 1);
        let refs: Vec<String> = update
            .supplement
            .models
            .iter()
            .map(|model| model.model_ref.as_string())
            .collect();
        assert_eq!(
            refs,
            vec![
                "xai/grok-new-9".to_string(),
                "xai/grok-retained".to_string()
            ]
        );
        assert_eq!(
            update.removed,
            vec![RemovedModel {
                model_ref: "xai/grok-vanished".into(),
                reason: RemovalReason::MissingUpstream,
            }]
        );
    }

    #[test]
    fn promotes_to_legacy_when_model_enters_builtin_catalog() {
        let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
        let mut prev = previous();
        let metadata = supplement_metadata("xai/grok-4.3", "Grok 4.3");
        prev.models.push(metadata);
        let update = generate(&parsed, Some(&prev), &legacy(), "2026-05-05", "rev", "ver").unwrap();
        assert!(update.removed.iter().any(|removed| {
            removed.model_ref == "xai/grok-4.3" && removed.reason == RemovalReason::PromotedToLegacy
        }));
        assert!(!update
            .supplement
            .models
            .iter()
            .any(|model| model.model_ref.as_string() == "xai/grok-4.3"));
    }

    #[test]
    fn defers_models_that_shadow_legacy_aliases() {
        let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
        let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();
        // mistral/magistral-small is a legacy alias; the snapshot lists it
        // under xai here as an unmapped id, so only the alias path matters.
        assert!(update
            .deferred
            .iter()
            .any(|deferred| { deferred.reason == DeferredReason::LegacyAliasConflict }));
    }

    #[test]
    fn supplement_round_trips_through_json() {
        let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
        let update = generate(
            &parsed,
            Some(&previous()),
            &legacy(),
            "2026-05-05",
            "rev",
            "ver",
        )
        .unwrap();
        let json = update.supplement.to_json().unwrap();
        let parsed_back = ModelsDevSupplement::parse(&json).unwrap();
        assert_eq!(parsed_back, update.supplement);
    }

    #[test]
    fn apply_to_rejects_collisions_and_inserts_routes() {
        let parsed: ModelsDevSnapshot = serde_json::from_str(&snapshot_json()).unwrap();
        let update = generate(&parsed, None, &legacy(), "2026-05-05", "rev", "ver").unwrap();
        let mut catalog = legacy();
        update.supplement.apply_to(&mut catalog).unwrap();
        let model_ref = ModelRef::parse("xai/grok-new-9").unwrap();
        assert!(catalog.get(&model_ref).is_some());
        let route = catalog.get_route(&ModelRouteRef::parse("xai@default/grok-new-9").unwrap());
        assert!(route.is_some());

        // Collision: force the supplement to claim a legacy model.
        let mut colliding = update.supplement.clone();
        let metadata = supplement_metadata("xai/grok-4.3", "Grok 4.3");
        colliding.models.push(metadata);
        let error = colliding.apply_to(&mut legacy()).unwrap_err();
        assert!(error.contains("collides with the built-in catalog"));
    }

    #[test]
    fn empty_bootstrap_parses_and_applies() {
        let json = serde_json::to_string_pretty(&ModelsDevSupplement::empty()).unwrap();
        let parsed = ModelsDevSupplement::parse(&json).unwrap();
        let mut catalog = legacy();
        parsed.apply_to(&mut catalog).unwrap();
        assert_eq!(catalog.list().len(), legacy().list().len());
    }
}
