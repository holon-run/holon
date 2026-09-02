use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use super::{
    BuiltInModelCatalog, BuiltInModelMetadata, BuiltInModelRoutePolicy, ModelRef, ModelRouteRef,
    ProviderId,
};

const SCHEMA_VERSION: u32 = 1;
const BUILT_IN_SNAPSHOT: &str = include_str!("builtin_snapshot_v1.json");
/// Checked-in `models.dev` supplemental catalog. Drafted by
/// `holon models-dev refresh` for allowlisted providers and admitted through
/// PR review; the runtime merges it into the built-in catalog.
const MODELS_DEV_SUPPLEMENT: &str = include_str!("../../models.dev/supplemental_catalog.json");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistrySnapshot {
    schema_version: u32,
    revision: String,
    models: Vec<BuiltInModelMetadata>,
    routes: Vec<SnapshotRoute>,
    aliases: Vec<SnapshotAlias>,
    preferred_models: Vec<SnapshotPreferredModel>,
    preferred_routes: Vec<SnapshotPreferredRoute>,
    preferred_routes_by_model: Vec<SnapshotPreferredModelRoute>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotRoute {
    route_ref: ModelRouteRef,
    policy: BuiltInModelRoutePolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotAlias {
    alias: ModelRef,
    target: ModelRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPreferredModel {
    provider: ProviderId,
    model_ref: ModelRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPreferredRoute {
    provider: ProviderId,
    route_ref: ModelRouteRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPreferredModelRoute {
    model_ref: ModelRef,
    route_ref: ModelRouteRef,
}

pub(super) fn built_in_catalog() -> Result<BuiltInModelCatalog, String> {
    let mut catalog = legacy_catalog()?;
    apply_models_dev_supplement(&mut catalog, MODELS_DEV_SUPPLEMENT)?;
    Ok(catalog)
}

/// The compiled-in built-in snapshot without the models.dev supplement.
/// Used as the drafting baseline by supplement generation and tests.
pub(super) fn legacy_catalog() -> Result<BuiltInModelCatalog, String> {
    parse_and_validate(BUILT_IN_SNAPSHOT)
}

fn apply_models_dev_supplement(catalog: &mut BuiltInModelCatalog, raw: &str) -> Result<(), String> {
    let supplement = crate::model_catalog::models_dev::supplement::ModelsDevSupplement::parse(raw)?;
    supplement.apply_to(catalog)
}

fn parse_and_validate(raw: &str) -> Result<BuiltInModelCatalog, String> {
    let snapshot = serde_json::from_str::<RegistrySnapshot>(raw)
        .map_err(|error| format!("failed to parse snapshot: {error}"))?;
    snapshot.validate()?;
    Ok(snapshot.into_catalog())
}

impl RegistrySnapshot {
    fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(format!(
                "unsupported schema version {}; expected {SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        if self.revision.trim().is_empty() {
            return Err("revision must not be empty".to_string());
        }

        let mut models = HashMap::new();
        for model in &self.models {
            validate_model_entry(model)?;
            if models.insert(model.model_ref.clone(), model).is_some() {
                return Err(format!("duplicate model {}", model.model_ref.as_string()));
            }
        }

        let mut routes = HashSet::new();
        for route in &self.routes {
            if !routes.insert(route.route_ref.clone()) {
                return Err(format!("duplicate route {}", route.route_ref.as_string()));
            }
            let model_ref = route.route_ref.model_ref();
            let model = models.get(&model_ref).ok_or_else(|| {
                format!(
                    "route {} references missing model {}",
                    route.route_ref.as_string(),
                    model_ref.as_string()
                )
            })?;
            route
                .policy
                .validate_narrowing(model)
                .map_err(|error| format!("route {}: {error}", route.route_ref.as_string()))?;
        }

        let mut aliases = HashMap::new();
        for alias in &self.aliases {
            if models.contains_key(&alias.alias) {
                return Err(format!(
                    "alias {} conflicts with a canonical model",
                    alias.alias.as_string()
                ));
            }
            if !models.contains_key(&alias.target) {
                return Err(format!(
                    "alias {} references missing model {}",
                    alias.alias.as_string(),
                    alias.target.as_string()
                ));
            }
            if aliases
                .insert(alias.alias.clone(), alias.target.clone())
                .is_some()
            {
                return Err(format!("duplicate alias {}", alias.alias.as_string()));
            }
        }
        for (alias, target) in &aliases {
            if aliases.contains_key(target) {
                return Err(format!(
                    "alias {} targets another alias {}",
                    alias.as_string(),
                    target.as_string()
                ));
            }
        }

        validate_preferred_models(&self.preferred_models, &models)?;
        validate_preferred_routes(&self.preferred_routes, &routes)?;

        let mut preferred_models = HashSet::new();
        for preferred in &self.preferred_routes_by_model {
            if !preferred_models.insert(preferred.model_ref.clone()) {
                return Err(format!(
                    "duplicate preferred route for model {}",
                    preferred.model_ref.as_string()
                ));
            }
            if !models.contains_key(&preferred.model_ref) {
                return Err(format!(
                    "preferred route references missing model {}",
                    preferred.model_ref.as_string()
                ));
            }
            if preferred.route_ref.model_ref() != preferred.model_ref {
                return Err(format!(
                    "preferred route {} does not match model {}",
                    preferred.route_ref.as_string(),
                    preferred.model_ref.as_string()
                ));
            }
            if !routes.contains(&preferred.route_ref) {
                return Err(format!(
                    "preferred route {} is not registered",
                    preferred.route_ref.as_string()
                ));
            }
        }
        Ok(())
    }

    fn into_catalog(self) -> BuiltInModelCatalog {
        BuiltInModelCatalog {
            entries: self
                .models
                .into_iter()
                .map(|model| (model.model_ref.clone(), model))
                .collect(),
            route_entries: self
                .routes
                .into_iter()
                .map(|route| (route.route_ref, route.policy))
                .collect(),
            aliases: self
                .aliases
                .into_iter()
                .map(|alias| (alias.alias, alias.target))
                .collect(),
            preferred_models: self
                .preferred_models
                .into_iter()
                .map(|preferred| (preferred.provider, preferred.model_ref))
                .collect(),
            preferred_routes: self
                .preferred_routes
                .into_iter()
                .map(|preferred| (preferred.provider, preferred.route_ref))
                .collect(),
            preferred_routes_by_model: self
                .preferred_routes_by_model
                .into_iter()
                .map(|preferred| (preferred.model_ref, preferred.route_ref))
                .collect(),
        }
    }

    #[cfg(test)]
    fn from_catalog(revision: impl Into<String>, catalog: &BuiltInModelCatalog) -> Self {
        let mut models = catalog.entries.values().cloned().collect::<Vec<_>>();
        models.sort_by_key(|model| model.model_ref.as_string());
        let mut routes = catalog
            .route_entries
            .iter()
            .map(|(route_ref, policy)| SnapshotRoute {
                route_ref: route_ref.clone(),
                policy: policy.clone(),
            })
            .collect::<Vec<_>>();
        routes.sort_by_key(|route| route.route_ref.as_string());
        let mut aliases = catalog
            .aliases
            .iter()
            .map(|(alias, target)| SnapshotAlias {
                alias: alias.clone(),
                target: target.clone(),
            })
            .collect::<Vec<_>>();
        aliases.sort_by_key(|alias| alias.alias.as_string());
        let mut preferred_models = catalog
            .preferred_models
            .iter()
            .map(|(provider, model_ref)| SnapshotPreferredModel {
                provider: provider.clone(),
                model_ref: model_ref.clone(),
            })
            .collect::<Vec<_>>();
        preferred_models.sort_by_key(|preferred| preferred.provider.as_str().to_string());
        let mut preferred_routes = catalog
            .preferred_routes
            .iter()
            .map(|(provider, route_ref)| SnapshotPreferredRoute {
                provider: provider.clone(),
                route_ref: route_ref.clone(),
            })
            .collect::<Vec<_>>();
        preferred_routes.sort_by_key(|preferred| preferred.provider.as_str().to_string());
        let mut preferred_routes_by_model = catalog
            .preferred_routes_by_model
            .iter()
            .map(|(model_ref, route_ref)| SnapshotPreferredModelRoute {
                model_ref: model_ref.clone(),
                route_ref: route_ref.clone(),
            })
            .collect::<Vec<_>>();
        preferred_routes_by_model.sort_by_key(|preferred| preferred.model_ref.as_string());
        Self {
            schema_version: SCHEMA_VERSION,
            revision: revision.into(),
            models,
            routes,
            aliases,
            preferred_models,
            preferred_routes,
            preferred_routes_by_model,
        }
    }
}

fn validate_preferred_models(
    preferred_models: &[SnapshotPreferredModel],
    models: &HashMap<ModelRef, &BuiltInModelMetadata>,
) -> Result<(), String> {
    let mut providers = HashSet::new();
    for preferred in preferred_models {
        if !providers.insert(preferred.provider.clone()) {
            return Err(format!(
                "duplicate preferred model for provider {}",
                preferred.provider.as_str()
            ));
        }
        if !models.contains_key(&preferred.model_ref) {
            return Err(format!(
                "preferred model {} is not registered",
                preferred.model_ref.as_string()
            ));
        }
    }
    Ok(())
}

fn validate_preferred_routes(
    preferred_routes: &[SnapshotPreferredRoute],
    routes: &HashSet<ModelRouteRef>,
) -> Result<(), String> {
    let mut providers = HashSet::new();
    for preferred in preferred_routes {
        if !providers.insert(preferred.provider.clone()) {
            return Err(format!(
                "duplicate preferred route for provider {}",
                preferred.provider.as_str()
            ));
        }
        if !routes.contains(&preferred.route_ref) {
            return Err(format!(
                "preferred route {} is not registered",
                preferred.route_ref.as_string()
            ));
        }
    }
    Ok(())
}

/// Per-model invariants shared by the built-in snapshot and the models.dev
/// supplemental catalog.
pub(super) fn validate_model_entry(model: &BuiltInModelMetadata) -> Result<(), String> {
    if model.model_ref.model.trim().is_empty() {
        return Err("model id must not be empty".to_string());
    }
    if model.endpoint.is_some() {
        return Err(format!(
            "canonical model {} must not declare an endpoint",
            model.model_ref.as_string()
        ));
    }
    if model.effective_context_window_percent == 0 || model.effective_context_window_percent > 100 {
        return Err(format!(
            "model {} has invalid effective context window percent {}",
            model.model_ref.as_string(),
            model.effective_context_window_percent
        ));
    }
    if model.context_window_tokens.is_some_and(|value| value == 0)
        || model
            .default_max_output_tokens
            .is_some_and(|value| value == 0)
        || model
            .max_output_tokens_upper_limit
            .is_some_and(|value| value == 0)
    {
        return Err(format!(
            "model {} token limits must be positive",
            model.model_ref.as_string()
        ));
    }
    if let (Some(default), Some(upper)) = (
        model.default_max_output_tokens,
        model.max_output_tokens_upper_limit,
    ) {
        if default > upper {
            return Err(format!(
                "model {} default max output {default} exceeds upper limit {upper}",
                model.model_ref.as_string()
            ));
        }
    }
    let mut reasoning_options = HashSet::new();
    if model
        .reasoning_effort_options
        .iter()
        .any(|option| !reasoning_options.insert(option))
    {
        return Err(format!(
            "model {} has duplicate reasoning effort options",
            model.model_ref.as_string()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "writes the checked-in built-in registry snapshot"]
    fn regenerate_builtin_snapshot() {
        let catalog = BuiltInModelCatalog::from_legacy_definitions();
        let snapshot = RegistrySnapshot::from_catalog("builtin-2026-08-28", &catalog);
        let json = serde_json::to_string_pretty(&snapshot).expect("snapshot must serialize");
        std::fs::write(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/src/model_catalog/builtin_snapshot_v1.json"
            ),
            format!("{json}\n"),
        )
        .expect("snapshot must be writable");
    }

    #[test]
    fn built_in_snapshot_matches_legacy_catalog() {
        let snapshot = legacy_catalog().expect("built-in snapshot must be valid");
        let legacy = BuiltInModelCatalog::from_legacy_definitions();
        assert_eq!(snapshot, legacy);
    }

    #[test]
    fn built_in_catalog_includes_checked_in_supplement() {
        let catalog = built_in_catalog().expect("built-in snapshot must be valid");
        let supplement = crate::model_catalog::models_dev::supplement::ModelsDevSupplement::parse(
            MODELS_DEV_SUPPLEMENT,
        )
        .expect("checked-in supplement must parse");
        for model in &supplement.models {
            let entry = catalog.get(&model.model_ref).unwrap_or_else(|| {
                panic!(
                    "supplement model {} must merge",
                    model.model_ref.as_string()
                )
            });
            assert_eq!(
                entry.source,
                crate::model_catalog::ModelMetadataSource::ModelsDevSupplement
            );
            let route = ModelRouteRef::new(
                model.model_ref.provider.clone(),
                crate::config::ProviderEndpointId::default_endpoint(),
                model.model_ref.model.clone(),
            );
            assert!(
                catalog.get_route(&route).is_some(),
                "supplement model {} must have a default route",
                model.model_ref.as_string()
            );
        }
    }

    #[test]
    fn rejects_unknown_schema_version() {
        let raw = BUILT_IN_SNAPSHOT.replacen("\"schema_version\": 1", "\"schema_version\": 999", 1);
        let error = parse_and_validate(&raw).expect_err("unknown version must fail");
        assert!(error.contains("unsupported schema version"));
    }

    #[test]
    fn rejects_unknown_fields() {
        let raw = BUILT_IN_SNAPSHOT.replacen(
            "\"revision\":",
            "\"unexpected\": true,\n  \"revision\":",
            1,
        );
        let error = parse_and_validate(&raw).expect_err("unknown fields must fail");
        assert!(error.contains("unknown field"));
    }

    #[test]
    fn rejects_duplicate_models() {
        let mut snapshot =
            serde_json::from_str::<RegistrySnapshot>(BUILT_IN_SNAPSHOT).expect("valid fixture");
        snapshot.models.push(snapshot.models[0].clone());
        let raw = serde_json::to_string(&snapshot).expect("snapshot must serialize");
        let error = parse_and_validate(&raw).expect_err("duplicate model must fail");
        assert!(error.contains("duplicate model"));
    }

    #[test]
    fn rejects_route_capability_expansion() {
        let mut snapshot =
            serde_json::from_str::<RegistrySnapshot>(BUILT_IN_SNAPSHOT).expect("valid fixture");
        let route = snapshot
            .routes
            .iter_mut()
            .find(|route| {
                let model = snapshot
                    .models
                    .iter()
                    .find(|model| model.model_ref == route.route_ref.model_ref())
                    .expect("route model");
                !model.capabilities.image_generation
            })
            .expect("route with unsupported image generation");
        route.policy.capabilities.image_generation = Some(true);
        let raw = serde_json::to_string(&snapshot).expect("snapshot must serialize");
        let error = parse_and_validate(&raw).expect_err("capability expansion must fail");
        assert!(error.contains("cannot enable intrinsic capability image_generation"));
    }
}
