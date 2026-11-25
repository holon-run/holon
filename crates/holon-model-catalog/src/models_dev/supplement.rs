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
//! review and merge; the supplement never changes credentials or provider
//! enablement.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::{
    BuiltInModelCatalog, BuiltInModelMetadata, BuiltInModelRoutePolicy, ModelMetadataSource,
};
use holon_domain::model_values::{ModelRef, ModelRouteRef, ProviderEndpointId};

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

/// Runtime providers that share an upstream models.dev catalog.
///
/// Shared metadata sources allow retention, not automatic endpoint admission.
/// New Codex aliases require endpoint review before entering the supplement.
const SUPPLEMENT_PROVIDER_ALIASES: &[(&str, &str)] = &[("openai", "openai-codex")];

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
    /// Explicitly reviewed non-default routes; never inferred from API availability.
    #[serde(default)]
    pub routes: Vec<ModelRouteRef>,
}

impl ModelsDevSupplement {
    /// The bootstrap supplement: valid, empty, no provenance.
    pub fn empty() -> Self {
        Self {
            schema_version: SUPPLEMENT_SCHEMA_VERSION,
            upstream_revision: String::new(),
            adapter_version: String::new(),
            models: Vec::new(),
            routes: Vec::new(),
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
        sorted.routes.sort_by_key(ModelRouteRef::as_string);
        serde_json::to_string_pretty(&sorted)
            .map_err(|error| format!("failed to serialize models.dev supplement: {error}"))
    }

    /// Structural validation independent of the legacy catalog.
    pub fn validate(&self) -> Result<(), String> {
        for model in &self.models {
            crate::snapshot::validate_model_entry(model)?;
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
        let model_refs: HashSet<ModelRef> = self
            .models
            .iter()
            .map(|model| model.model_ref.clone())
            .collect();
        let mut seen_routes = HashSet::new();
        for route in &self.routes {
            if route.endpoint == ProviderEndpointId::default_endpoint() {
                return Err(format!(
                    "supplement route {} must not use the default endpoint",
                    route.as_string()
                ));
            }
            if !model_refs.contains(&route.model_ref()) {
                return Err(format!(
                    "supplement route {} references missing model {}",
                    route.as_string(),
                    route.model_ref().as_string()
                ));
            }
            if !seen_routes.insert(route.clone()) {
                return Err(format!("duplicate supplement route {}", route.as_string()));
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
        for route_ref in &self.routes {
            if catalog
                .route_entries
                .insert(route_ref.clone(), BuiltInModelRoutePolicy::default())
                .is_some()
            {
                return Err(format!(
                    "supplement route {} collides with a built-in route",
                    route_ref.as_string()
                ));
            }
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
    /// The model uses modalities unsupported by the target runtime provider.
    UnsupportedByProvider,
    /// A shared upstream catalog does not establish endpoint access.
    EndpointReviewRequired,
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
    /// The model is not supported by the target runtime provider.
    UnsupportedByProvider,
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
    let mut removed: Vec<RemovedModel> = Vec::new();
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
        for holon_provider_id in
            supplement_provider_targets(md_provider_id, &mapping.holon_provider_id)
        {
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
                if !supplement_model_supported_by_provider(&holon_provider_id, md_model_id, model) {
                    if remaining_previous.remove(&model_ref_string) {
                        removed.push(RemovedModel {
                            model_ref: model_ref_string,
                            reason: RemovalReason::UnsupportedByProvider,
                        });
                    } else {
                        deferred.push(DeferredModel {
                            model_ref: model_ref_string,
                            reason: DeferredReason::UnsupportedByProvider,
                            release_date: release_date_key(model).map(str::to_string),
                        });
                    }
                    continue;
                }
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
                // Native limits and effort metadata still need endpoint review.
                if holon_provider_id == "mistral"
                    && matches!(
                        md_model_id.as_str(),
                        "deepseek-v4-1-flash-1017" | "mistral-large-4-0-8"
                    )
                {
                    remaining_previous.remove(&model_ref_string);
                    deferred.push(DeferredModel {
                        model_ref: model_ref_string,
                        reason: DeferredReason::EndpointReviewRequired,
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
                if holon_provider_id == "openai-codex" {
                    deferred.push(DeferredModel {
                        model_ref: model_ref_string,
                        reason: DeferredReason::EndpointReviewRequired,
                        release_date: release_date_key(model).map(str::to_string),
                    });
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
    }

    // Retained entries need re-projection from the current snapshot; collect
    // them from the snapshot loop above is awkward, so rebuild here from the
    // retained ref list.
    let mut models: Vec<BuiltInModelMetadata> = drafted.clone();
    for model_ref_string in &retained {
        let model_ref = ModelRef::parse(model_ref_string)
            .map_err(|error| format!("invalid retained model ref {model_ref_string}: {error}"))?;
        let md_provider_id = supplement_source_provider_id(&mappings, model_ref.provider.as_str())
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

    removed.extend(
        remaining_previous
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
            .collect::<Vec<_>>(),
    );
    removed.extend(promoted.into_iter().map(|model_ref| RemovedModel {
        model_ref,
        reason: RemovalReason::PromotedToLegacy,
    }));

    drafted.sort_by_key(|model| model.model_ref.as_string());
    models.sort_by_key(|model| model.model_ref.as_string());
    let current_model_refs: HashSet<ModelRef> =
        models.iter().map(|model| model.model_ref.clone()).collect();
    let mut routes = Vec::new();
    let mut route_keys = HashSet::new();
    if let Some(previous) = previous {
        for route in &previous.routes {
            if current_model_refs.contains(&route.model_ref())
                // Both subscriptions retired K2.6 on 2026-08-18, K2.7 Code
                // on 2026-10-28, and MiniMax M3 on 2026-11-02.
                // This does not establish retirement on the ordinary API.
                && !(route.provider.as_str() == "volcengine"
                    && matches!(route.endpoint.as_str(), "coding" | "plan")
                    && matches!(
                        route.model.as_str(),
                        "kimi-k2.6" | "kimi-k2.7-code" | "minimax-m3"
                    ))
                && route_keys.insert(route.as_string())
            {
                routes.push(route.clone());
            }
        }
    }
    // API catalog availability does not establish subscription endpoint support.
    // New non-default routes must be checked against endpoint-specific evidence
    // and explicitly added to the reviewed supplement.
    routes.sort_by_key(ModelRouteRef::as_string);
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
            routes,
        },
        drafted,
        retained,
        removed,
        deferred,
        providers_not_allowlisted,
    })
}

fn supplement_provider_targets(md_provider_id: &str, holon_provider_id: &str) -> Vec<String> {
    let mut targets = vec![holon_provider_id.to_string()];
    targets.extend(
        SUPPLEMENT_PROVIDER_ALIASES
            .iter()
            .filter(|(source, _)| *source == md_provider_id)
            .map(|(_, target)| (*target).to_string()),
    );
    targets
}

fn supplement_model_supported_by_provider(
    provider: &str,
    model_id: &str,
    model: &ModelsDevModel,
) -> bool {
    // Audio-only Live API transcripts are not GenerateContent text turns.
    // https://ai.google.dev/gemini-api/docs/models/gemini-3.5-live-translate-preview
    if provider == "gemini" && model_id == "gemini-3.5-live-translate-preview" {
        return false;
    }
    // Every supplement model implies a default API route. These EOS dates do
    // not retire independently registered coding/plan or hosted offerings.
    // https://ark.volcengine.com/region:cn-beijing/docs/model-deprecation-notice.md
    if provider == "volcengine"
        && matches!(
            model_id,
            "deepseek-v3-2-251201"
                | "doubao-seed-1-8-251228"
                | "doubao-seed-1-6-251015"
                | "doubao-seed-1-6-flash-250828"
                | "doubao-seed-1-6-vision-250815"
                | "doubao-seed-2-0-lite-260215"
                | "doubao-seed-2-0-mini-260215"
                | "doubao-seed-2-0-code-preview-260215"
        )
    {
        return false;
    }
    // International native API retirement, not a family-wide aggregator ban.
    // https://platform.kimi.ai/docs/models.md (retired 2026-08-31)
    if provider == "moonshot"
        && matches!(
            model_id,
            "kimi-k2.5"
                | "moonshot-v1-8k"
                | "moonshot-v1-32k"
                | "moonshot-v1-128k"
                | "moonshot-v1-auto"
                | "moonshot-v1-8k-vision-preview"
                | "moonshot-v1-32k-vision-preview"
                | "moonshot-v1-128k-vision-preview"
        )
    {
        return false;
    }
    // Native Chat IDs retired on 2026-10-21; aggregator and audio IDs are separate.
    // https://mimo.mi.com/docs/en-US/updates/deprecate
    if provider == "xiaomi" && matches!(model_id, "mimo-v2.5-pro" | "mimo-v2.5") {
        return false;
    }
    // Explicitly retired on the international pay-as-you-go API. The M2.5/M2.7
    // retirement on 2026-11-18 does not apply to subscription compatibility names.
    // https://platform.minimax.io/docs/faq/models.md
    if provider == "minimax"
        && matches!(
            model_id,
            "MiniMax-M2"
                | "MiniMax-M2.1"
                | "MiniMax-M2.1-highspeed"
                | "MiniMax-M2.5"
                | "MiniMax-M2.5-highspeed"
                | "MiniMax-M2.7"
                | "MiniMax-M2.7-highspeed"
        )
    {
        return false;
    }
    if provider != "openai-codex" {
        return true;
    }
    // Retired for ChatGPT sign-in, not for API-key sessions.
    // https://learn.chatgpt.com/docs/models#deprecated-codex-models
    if matches!(
        model_id,
        "gpt-5.2"
            | "gpt-5.3-codex"
            | "gpt-5.3-codex-spark"
            | "gpt-5.4"
            | "gpt-5.4-mini"
            | "gpt-5.5"
    ) {
        return false;
    }
    model.modalities.as_ref().is_none_or(|modalities| {
        !modalities
            .input
            .iter()
            .chain(&modalities.output)
            .any(|modality| modality == "audio")
    })
}

fn supplement_source_provider_id(
    mappings: &BTreeMap<&str, &ProviderMapping>,
    holon_provider_id: &str,
) -> Option<String> {
    mappings.iter().find_map(|(md_provider_id, mapping)| {
        if mapping.holon_provider_id == holon_provider_id
            || SUPPLEMENT_PROVIDER_ALIASES
                .iter()
                .any(|(source, target)| *source == *md_provider_id && *target == holon_provider_id)
        {
            Some((*md_provider_id).to_string())
        } else {
            None
        }
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
mod tests;
